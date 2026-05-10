//! ALSA playback worker.
//!
//! Opens a PCM device for playback in the format declared by the wire header
//! and drains a bounded byte channel into ALSA. Underrun (`EPIPE`) is recovered
//! transparently via `pcm.try_recover`. The worker exits cleanly when the
//! channel is closed, draining whatever is left.

use std::time::{Duration, Instant};

use alsa::pcm::{Access, Format as AFormat, HwParams};
use alsa::{Direction, PCM, ValueOr};
use anyhow::{Context, Result};
use crossbeam_channel::Receiver;
use proto::{Format, Header};
use tracing::{debug, info, warn};

/// Open and configure the playback PCM. Returns the PCM plus the negotiated
/// `period_size` in frames so the writer can compute prefill targets.
pub struct AlsaPlayback {
    pcm: PCM,
    bytes_per_frame: usize,
}

impl AlsaPlayback {
    pub fn open(device: &str, header: &Header, buffer_ms: u32) -> Result<Self> {
        // Target ~4 periods per buffer.
        let buffer_frames = u64::from(header.sample_rate) * u64::from(buffer_ms) / 1000;
        let period_frames = (buffer_frames / 4).max(64);

        // Retry: just after we ask CamillaDSP to swap configs, the loopback
        // playback half is still constrained to whatever format the *previous*
        // capture used. CamillaDSP takes up to a couple of seconds to re-open
        // its capture with the new format, after which our open succeeds.
        let pcm = open_configured_with_retry(
            device,
            header,
            buffer_frames,
            period_frames,
            Duration::from_secs(3),
        )?;

        let actual_buf = pcm
            .hw_params_current()
            .and_then(|h| h.get_buffer_size())
            .unwrap_or(buffer_frames as i64);
        let actual_period = pcm
            .hw_params_current()
            .and_then(|h| h.get_period_size())
            .unwrap_or(period_frames as i64);

        let bytes_per_frame = header.frame_bytes();
        info!(
            device,
            sample_rate = header.sample_rate,
            channels = header.channels,
            format = ?header.format,
            buffer_frames = actual_buf,
            period_frames = actual_period,
            bytes_per_frame,
            "ALSA configured",
        );

        // actual_period/actual_buf are observed only to log the negotiated
        // values; the writer doesn't need them after configuration.
        let _ = (actual_buf, actual_period, period_frames, buffer_frames);

        Ok(Self {
            pcm,
            bytes_per_frame,
        })
    }

    /// Drain `rx` into ALSA until the channel closes. The worker will block
    /// inside `writei` while ALSA buffer is full; this is the desired
    /// back-pressure path.
    pub fn run(self, rx: Receiver<Vec<u8>>) -> Result<()> {
        let pcm = self.pcm;
        let bpf = self.bytes_per_frame;
        let io = pcm.io_bytes();
        // io_bytes returns a wrapper, no Result — see alsa-rs IO impls.

        // Single rolling buffer holding the unwritten remainder of a chunk —
        // ALSA may consume only a partial number of frames per writei call.
        let mut leftover: Vec<u8> = Vec::new();

        loop {
            // Pull either leftover bytes or a fresh chunk from the network.
            let chunk = if leftover.is_empty() {
                match rx.recv() {
                    Ok(c) => c,
                    Err(_) => break, // channel closed → graceful end
                }
            } else {
                std::mem::take(&mut leftover)
            };

            let mut offset = 0usize;
            while offset < chunk.len() {
                // Round down to whole frames; if a fragment is partial, hold
                // it until the next chunk so writei never sees a torn frame.
                let usable = (chunk.len() - offset) / bpf * bpf;
                if usable == 0 {
                    leftover = chunk[offset..].to_vec();
                    break;
                }
                let slice = &chunk[offset..offset + usable];
                match io.writei(slice) {
                    Ok(frames) => {
                        offset += frames * bpf;
                    }
                    Err(e) => {
                        warn!(error = %e, errno = e.errno(), "ALSA writei error → try_recover");
                        if let Err(re) = pcm.try_recover(e, false) {
                            return Err(anyhow::anyhow!("ALSA unrecoverable: {re}"));
                        }
                    }
                }
            }
        }

        debug!("ALSA worker draining device");
        let _ = pcm.drain();
        Ok(())
    }
}

const fn alsa_format(f: Format) -> AFormat {
    match f {
        Format::S16LE => AFormat::S16LE,
        Format::S32LE => AFormat::S32LE,
        Format::F32LE => AFormat::FloatLE,
    }
}

/// Open and fully configure the playback PCM, retrying while ALSA reports the
/// requested format unavailable. snd-aloop reflects whatever format its
/// capture half opened, so just after a CamillaDSP config reload the playback
/// half can take up to a couple of seconds to expose the new format.
fn open_configured_with_retry(
    device: &str,
    header: &Header,
    buffer_frames: u64,
    period_frames: u64,
    total_timeout: Duration,
) -> Result<PCM> {
    let started = Instant::now();
    let mut last_err: Option<String> = None;

    loop {
        match try_configure(device, header, buffer_frames, period_frames) {
            Ok(pcm) => return Ok(pcm),
            Err(e) => {
                let msg = format!("{e}");
                let elapsed = started.elapsed();
                if elapsed >= total_timeout {
                    return Err(anyhow::anyhow!(
                        "opening ALSA `{device}` after {:?}: {} (last error)",
                        elapsed,
                        last_err.unwrap_or(msg),
                    ));
                }
                debug!(error = %msg, "ALSA configure failed, retrying");
                last_err = Some(msg);
                std::thread::sleep(Duration::from_millis(150));
            }
        }
    }
}

fn try_configure(
    device: &str,
    header: &Header,
    buffer_frames: u64,
    period_frames: u64,
) -> Result<PCM> {
    let pcm = PCM::new(device, Direction::Playback, false)
        .with_context(|| format!("opening `{device}`"))?;
    {
        let hwp = HwParams::any(&pcm).context("HwParams::any")?;
        hwp.set_channels(u32::from(header.channels))
            .context("set_channels")?;
        hwp.set_rate(header.sample_rate, ValueOr::Nearest)
            .context("set_rate")?;
        hwp.set_format(alsa_format(header.format))
            .context("set_format")?;
        hwp.set_access(Access::RWInterleaved)
            .context("set_access(RWInterleaved)")?;
        hwp.set_buffer_size_near(buffer_frames as i64)
            .context("set_buffer_size_near")?;
        hwp.set_period_size_near(period_frames as i64, ValueOr::Nearest)
            .context("set_period_size_near")?;
        pcm.hw_params(&hwp).context("apply hw_params")?;
    }
    Ok(pcm)
}
