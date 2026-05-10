//! cpal capture → wire-format byte chunks.
//!
//! The audio callback never blocks: each batch of captured samples is converted
//! to the configured wire format and pushed onto a bounded channel. If the
//! network side falls behind we drop the oldest chunk and log it — keeping the
//! audio thread real-time-safe is non-negotiable.

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{Device, FromSample, Host, Sample, SampleFormat, Stream, StreamConfig};
use crossbeam_channel::{Sender, TrySendError};
use proto::Format;
use tracing::{info, warn};

use crate::activity::ActivityTracker;

/// Best-effort human label for a cpal device, preferring the new `description`
/// API and falling back to the deprecated `name` accessor when that's all the
/// host backend exposes. Used for both matching by name and logging.
pub fn device_label(d: &Device) -> String {
    if let Ok(desc) = d.description() {
        return desc.name().to_string();
    }
    #[allow(deprecated)]
    d.name().unwrap_or_else(|_| "<unknown>".into())
}

pub struct CaptureSpec {
    pub rate: u32,
    pub channels: u16,
    pub wire_format: Format,
}

/// Resolve `host_name` (or default) and `device_name` (or "default") to a
/// concrete cpal device. Names are matched case-insensitively against
/// `device.name()`.
pub fn pick_device(host_name: Option<&str>, device_name: &str) -> Result<(Host, Device)> {
    let host = match host_name {
        None => cpal::default_host(),
        Some(name) => {
            let id = cpal::available_hosts()
                .into_iter()
                .find(|id| id.name().eq_ignore_ascii_case(name))
                .ok_or_else(|| anyhow!("no cpal host named `{name}`"))?;
            cpal::host_from_id(id).context("opening named cpal host")?
        }
    };

    let device = if device_name == "default" {
        host.default_input_device()
            .ok_or_else(|| anyhow!("host has no default input device"))?
    } else {
        host.input_devices()
            .context("listing input devices")?
            .find(|d| device_label(d).eq_ignore_ascii_case(device_name))
            .ok_or_else(|| anyhow!("no input device named `{device_name}`"))?
    };

    Ok((host, device))
}

/// Open an input stream that delivers samples to `tx` already encoded as the
/// configured wire format. The returned `Stream` must be kept alive — dropping
/// it stops capture. Each chunk's peak amplitude is also reported to
/// `activity` so the main thread can engage / disengage the TCP path
/// based on whether anything is actually playing.
pub fn open_capture(
    device: &Device,
    spec: &CaptureSpec,
    tx: Sender<Vec<u8>>,
    activity: ActivityTracker,
) -> Result<Stream> {
    // Walk supported configs to find one compatible with our requested
    // (channels, rate). When multiple match, prefer the highest-fidelity
    // sample format — many cpal/ALSA hosts list U8/I8 first which is the
    // worst possible choice for a music bridge.
    let chosen = device
        .supported_input_configs()
        .context("supported_input_configs")?
        .filter(|range| {
            let ch_ok = u32::from(spec.channels) == u32::from(range.channels());
            let rate = spec.rate;
            let rate_ok = rate >= range.min_sample_rate() && rate <= range.max_sample_rate();
            ch_ok && rate_ok
        })
        .max_by_key(|range| format_quality(range.sample_format()))
        .map(|range| range.with_sample_rate(spec.rate));

    let supported = if let Some(c) = chosen {
        c
    } else {
        let d = device
            .default_input_config()
            .context("default_input_config")?;
        warn!(
            requested_rate = spec.rate,
            requested_channels = spec.channels,
            actual_rate = d.sample_rate(),
            actual_channels = d.channels(),
            "device does not support requested config — falling back to default",
        );
        d
    };

    let device_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    info!(
        device_format = ?device_format,
        wire_format = ?spec.wire_format,
        rate = config.sample_rate,
        channels = config.channels,
        "input stream opening",
    );

    let err_fn = |e| warn!(error = %e, "cpal stream error");
    let wire = spec.wire_format;

    let stream = match device_format {
        SampleFormat::F32 => {
            let act = activity.clone();
            device.build_input_stream(
                &config,
                move |data: &[f32], _| forward(data, wire, &tx, &act),
                err_fn,
                None,
            )
        }
        SampleFormat::I32 => {
            let act = activity.clone();
            device.build_input_stream(
                &config,
                move |data: &[i32], _| forward(data, wire, &tx, &act),
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let act = activity.clone();
            device.build_input_stream(
                &config,
                move |data: &[i16], _| forward(data, wire, &tx, &act),
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let act = activity.clone();
            device.build_input_stream(
                &config,
                move |data: &[u16], _| forward_u16(data, wire, &tx, &act),
                err_fn,
                None,
            )
        }
        SampleFormat::U8 => {
            let act = activity.clone();
            device.build_input_stream(
                &config,
                move |data: &[u8], _| forward_u8(data, wire, &tx, &act),
                err_fn,
                None,
            )
        }
        SampleFormat::I8 => {
            let act = activity.clone();
            device.build_input_stream(
                &config,
                move |data: &[i8], _| forward(data, wire, &tx, &act),
                err_fn,
                None,
            )
        }
        other => bail!("unsupported cpal sample format: {other:?}"),
    }
    .context("build_input_stream")?;

    Ok(stream)
}

fn peak_abs<T>(samples: &[T]) -> f32
where
    T: Sample + Copy,
    f32: FromSample<T>,
{
    let mut peak: f32 = 0.0;
    for &s in samples {
        let v = f32::from_sample(s).abs();
        if v > peak {
            peak = v;
        }
    }
    peak
}

fn forward<T>(samples: &[T], wire: Format, tx: &Sender<Vec<u8>>, activity: &ActivityTracker)
where
    T: Sample + Copy,
    i16: FromSample<T>,
    i32: FromSample<T>,
    f32: FromSample<T>,
{
    if samples.is_empty() {
        return;
    }
    let peak = peak_abs(samples);
    activity.note_peak(peak);
    let bytes = encode(samples, wire);
    push(tx, bytes, peak);
}

fn forward_u16(samples: &[u16], wire: Format, tx: &Sender<Vec<u8>>, activity: &ActivityTracker) {
    if samples.is_empty() {
        return;
    }
    // cpal U16 is unsigned-PCM; FromSample only goes via sample::Sample types.
    // Hand-convert to i16 first for a uniform path.
    let mut as_i16: Vec<i16> = Vec::with_capacity(samples.len());
    for &s in samples {
        as_i16.push((s as i32 - i32::from(i16::MAX) - 1) as i16);
    }
    let peak = peak_abs(&as_i16);
    activity.note_peak(peak);
    let bytes = encode(&as_i16, wire);
    push(tx, bytes, peak);
}

fn forward_u8(samples: &[u8], wire: Format, tx: &Sender<Vec<u8>>, activity: &ActivityTracker) {
    if samples.is_empty() {
        return;
    }
    // U8 is unsigned-PCM with bias 128; convert to i8 first for FromSample.
    let mut as_i8: Vec<i8> = Vec::with_capacity(samples.len());
    for &s in samples {
        as_i8.push((s as i16 - 128) as i8);
    }
    let peak = peak_abs(&as_i8);
    activity.note_peak(peak);
    let bytes = encode(&as_i8, wire);
    push(tx, bytes, peak);
}

/// Score sample formats by music-bridge fidelity. Higher = better.
const fn format_quality(f: SampleFormat) -> u8 {
    match f {
        SampleFormat::F64 => 60,
        SampleFormat::F32 => 50,
        SampleFormat::I32 | SampleFormat::U32 => 40,
        SampleFormat::I16 | SampleFormat::U16 => 30,
        SampleFormat::I8 | SampleFormat::U8 => 10,
        _ => 1,
    }
}

fn encode<T>(samples: &[T], wire: Format) -> Vec<u8>
where
    T: Sample + Copy,
    i16: FromSample<T>,
    i32: FromSample<T>,
    f32: FromSample<T>,
{
    let bps = wire.bytes_per_sample();
    let mut out = Vec::with_capacity(samples.len() * bps);
    match wire {
        Format::S16LE => {
            for &s in samples {
                let v = i16::from_sample(s);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Format::S32LE => {
            for &s in samples {
                let v = i32::from_sample(s);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        Format::F32LE => {
            for &s in samples {
                let v = f32::from_sample(s);
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
    }
    out
}

fn push(tx: &Sender<Vec<u8>>, chunk: Vec<u8>, peak: f32) {
    // The audio callback must never block. If the network queue is full we
    // drop the newest chunk — the alternative (blocking inside cpal) would
    // glitch the source application's playback worse.
    match tx.try_send(chunk) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => {
            // Smart-sleep silently fills the channel with silent chunks
            // while the TCP path is released; warning every 10 ms in
            // that state turns the log into noise. Only complain when
            // the chunk had real audio content (peak above ~-60 dBFS).
            if peak > 0.001 {
                warn!("network queue full → dropped chunk");
            }
        }
        Err(TrySendError::Disconnected(_)) => {}
    }
}
