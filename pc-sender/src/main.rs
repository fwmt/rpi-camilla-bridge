//! pc-sender: capture local audio via cpal and stream PCM to pi-receiver over TCP.

mod audio_in;
mod net_out;
#[cfg(target_os = "linux")]
mod virtual_sink;

use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use audio_in::device_label;
use clap::{Parser, Subcommand, ValueEnum};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::bounded;
use proto::{Format, Header};
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "rpi-camilla-bridge: PC audio → Raspberry Pi sender")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Pi hostname or IP.
    #[arg(long, default_value = "rpi.local", global = true)]
    host: String,

    /// pi-receiver TCP port.
    #[arg(long, default_value_t = 9000, global = true)]
    port: u16,

    /// Input device name (case-insensitive). Use "default" for the host
    /// default. On Linux, the default behavior also creates a PulseAudio /
    /// PipeWire virtual output called "rpi-camilla-bridge" so the bridge
    /// shows up in Settings → Sound → Output; pass any other name here to
    /// skip that and capture from a specific cpal source.
    #[arg(long, default_value = "default", global = true)]
    device: String,

    /// Linux only: skip creating the `rpi-camilla-bridge` virtual output
    /// and capture from cpal's default input (typically your microphone).
    /// Useful when you already route audio yourself or the target box has
    /// no PulseAudio / PipeWire daemon running.
    #[arg(long, global = true, default_value_t = false)]
    no_virtual_sink: bool,

    /// Optional cpal host to pin (e.g. "alsa", "wasapi"). Empty = platform default.
    #[arg(long, global = true)]
    cpal_host: Option<String>,

    /// Sample rate in Hz to request from the input device. 48 kHz matches the
    /// default shared-mode mixer rate on both Windows (WASAPI) and Linux
    /// (PipeWire/PulseAudio) so it works out-of-the-box on most systems.
    /// CamillaDSP resamples to its own internal rate (96 kHz) using its
    /// high-quality AsyncSinc engine — no PC-side resampling needed.
    #[arg(long, default_value_t = 48_000, global = true)]
    rate: u32,

    /// Channel count to request.
    #[arg(long, default_value_t = 2, global = true)]
    channels: u16,

    /// Wire format the bridge expects on the Pi side. The `bridge.yml` capture
    /// section on the Pi must match this format exactly.
    #[arg(long, value_enum, default_value_t = WireFormat::S32le, global = true)]
    format: WireFormat,

    /// Initial reconnect delay in milliseconds. Doubles up to 5 s on consecutive failures.
    #[arg(long, default_value_t = 1_000, global = true)]
    reconnect_ms: u64,

    /// Log level (trace, debug, info, warn, error). Overridden by `RUST_LOG`.
    #[arg(long, default_value = "info", global = true)]
    log_level: String,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// List cpal hosts and their input devices, with supported configurations.
    ListDevices,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum WireFormat {
    S16le,
    S32le,
    F32le,
}

impl From<WireFormat> for Format {
    fn from(w: WireFormat) -> Self {
        match w {
            WireFormat::S16le => Self::S16LE,
            WireFormat::S32le => Self::S32LE,
            WireFormat::F32le => Self::F32LE,
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli.log_level);

    match &cli.cmd {
        Some(Cmd::ListDevices) => list_devices(),
        None => run(cli),
    }
}

fn init_logging(default_level: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default_level));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(true)
        .compact()
        .init();
}

fn list_devices() -> Result<()> {
    for host_id in cpal::available_hosts() {
        println!("host: {}", host_id.name());
        let host = match cpal::host_from_id(host_id) {
            Ok(h) => h,
            Err(e) => {
                println!("  (failed to open host: {e})");
                continue;
            }
        };
        if let Some(d) = host.default_input_device() {
            println!("  default input: {}", device_label(&d));
        }
        match host.input_devices() {
            Ok(it) => {
                for dev in it {
                    println!("  device: {}", device_label(&dev));
                    if let Ok(cfgs) = dev.supported_input_configs() {
                        for c in cfgs {
                            println!(
                                "    {:?} ch={} rate={}..{} buffer={:?}",
                                c.sample_format(),
                                c.channels(),
                                c.min_sample_rate(),
                                c.max_sample_rate(),
                                c.buffer_size(),
                            );
                        }
                    }
                }
            }
            Err(e) => println!("  (error listing input devices: {e})"),
        }
    }
    Ok(())
}

fn run(cli: Cli) -> Result<()> {
    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = Arc::clone(&stop);
        ctrlc::set_handler(move || {
            info!("signal received, shutting down");
            s.store(true, Ordering::SeqCst);
        })
        .context("installing ctrl-c handler")?;
    }

    // Linux: register a virtual output so the bridge appears in the OS
    // Sound settings as "rpi-camilla-bridge". Held until `run` returns;
    // its Drop impl unloads the sink and restores the previous default
    // source. Other platforms get whatever cpal exposes as default.
    #[cfg(target_os = "linux")]
    let _virtual_sink = (!cli.no_virtual_sink && cli.device == "default")
        .then(|| {
            virtual_sink::VirtualSink::try_create()
                .map_err(|e| warn!(error = %e, "could not register virtual output; falling back to cpal default"))
                .ok()
                .flatten()
        })
        .flatten();

    let (_host, device) = audio_in::pick_device(cli.cpal_host.as_deref(), &cli.device)
        .context("selecting cpal input device")?;
    info!(name = device_label(&device), "using input device");

    let spec = audio_in::CaptureSpec {
        rate: cli.rate,
        channels: cli.channels,
        wire_format: cli.format.into(),
    };

    // The audio side stays up across reconnects; only the TCP side is rebuilt.
    let (tx, rx) = bounded::<Vec<u8>>(64);
    let stream = audio_in::open_capture(&device, &spec, tx).context("opening capture")?;
    stream.play().context("starting capture stream")?;

    let mut backoff_ms = cli.reconnect_ms;
    while !stop.load(Ordering::SeqCst) {
        match try_session(&cli, &spec, &rx, &stop) {
            Ok(()) => backoff_ms = cli.reconnect_ms,
            Err(e) => {
                warn!(error = %e, "session ended; will reconnect");
            }
        }
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let sleep_ms = backoff_ms.min(5_000);
        info!(reconnect_in_ms = sleep_ms, "sleeping before reconnect");
        let until = std::time::Instant::now() + Duration::from_millis(sleep_ms);
        while std::time::Instant::now() < until && !stop.load(Ordering::SeqCst) {
            std::thread::sleep(Duration::from_millis(50));
        }
        backoff_ms = (backoff_ms * 2).min(5_000);
    }
    drop(stream);
    Ok(())
}

fn try_session(
    cli: &Cli,
    spec: &audio_in::CaptureSpec,
    rx: &crossbeam_channel::Receiver<Vec<u8>>,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    info!(host = %cli.host, port = cli.port, "connecting");
    let mut stream =
        TcpStream::connect_timeout(&resolve_one(&cli.host, cli.port)?, Duration::from_secs(5))
            .context("TCP connect")?;
    stream.set_nodelay(true).ok();
    stream.set_write_timeout(Some(Duration::from_secs(5))).ok();

    let header = Header {
        format: spec.wire_format,
        channels: spec.channels,
        sample_rate: spec.rate,
    };
    header.write_to(&mut stream).context("writing header")?;
    info!(
        format = ?header.format,
        channels = header.channels,
        sample_rate = header.sample_rate,
        "header sent",
    );

    // Drain any chunks that piled up while disconnected so we start the new
    // connection close to real-time instead of catching up with stale audio.
    let mut drained = 0usize;
    while rx.try_recv().is_ok() {
        drained += 1;
    }
    if drained > 0 {
        info!(
            dropped_chunks = drained,
            "discarded stale audio queued during disconnect"
        );
    }

    // Run the writer on its own thread so the reconnect loop above sees the
    // error path. Two complementary shutdown paths cover Ctrl+C cleanly:
    //   1. The writer self-polls `stop` via `recv_timeout(100 ms)`, so an
    //      idle channel never wedges the exit.
    //   2. This thread polls `stop` and hits `shutdown(Both)` on the TCP
    //      socket; a `write_all` blocked on a full kernel send buffer then
    //      returns immediately with an error, instead of waiting out the
    //      5 s write timeout.
    let stream_for_shutdown = stream.try_clone().context("clone TCP stream")?;
    let rx_clone = rx.clone();
    let stop_clone = Arc::clone(stop);
    let writer = std::thread::Builder::new()
        .name("net-writer".into())
        .spawn(move || net_out::pump(stream, rx_clone, stop_clone))
        .context("spawning net writer")?;

    while !writer.is_finished() {
        if stop.load(Ordering::SeqCst) {
            let _ = stream_for_shutdown.shutdown(std::net::Shutdown::Both);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    let total = writer
        .join()
        .map_err(|_| anyhow::anyhow!("writer panicked"))??;
    info!(bytes_sent = total, "session ended");
    Ok(())
}

fn resolve_one(host: &str, port: u16) -> Result<std::net::SocketAddr> {
    use std::net::ToSocketAddrs;
    (host, port)
        .to_socket_addrs()
        .with_context(|| format!("resolving {host}:{port}"))?
        .next()
        .ok_or_else(|| anyhow::anyhow!("DNS returned no addresses for {host}:{port}"))
}
