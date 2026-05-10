//! pi-receiver: TCP → ALSA bridge for CamillaDSP.
//!
//! Listens on a TCP port, reads the 16-byte rpi-camilla-bridge header, opens
//! ALSA playback against the loopback configured in CamillaDSP, and pumps PCM
//! frames from the socket into ALSA. While a connection is active, switches
//! CamillaDSP to a "bridge" config via websocket so the capture format matches
//! what the PC sender is producing; restores the idle config on disconnect.

mod alsa_out;
mod camilla_ws;
mod discovery;
mod doctor;
mod init;
mod net_in;

use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossbeam_channel::bounded;
use proto::Header;
use tracing::{info, warn};
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(version, about = "rpi-camilla-bridge: PC → CamillaDSP TCP receiver")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// TCP port to listen on (binds 0.0.0.0).
    #[arg(long, default_value_t = 9000, global = true)]
    port: u16,

    /// ALSA playback device (the loopback half CamillaDSP captures from).
    #[arg(long, default_value = "hw:Loopback,0,0", global = true)]
    device: String,

    /// Target ALSA buffer in milliseconds.
    #[arg(long, default_value_t = 200, global = true)]
    buffer_ms: u32,

    /// CamillaDSP websocket host. Empty disables config switching.
    #[arg(long, default_value = "127.0.0.1", global = true)]
    camilla_host: String,

    /// CamillaDSP websocket port.
    #[arg(long, default_value_t = 1234, global = true)]
    camilla_port: u16,

    /// Path to the CamillaDSP config used while the bridge is active.
    #[arg(long, global = true)]
    bridge_config: Option<String>,

    /// Path to the CamillaDSP config restored when the bridge is idle.
    #[arg(long, global = true)]
    idle_config: Option<String>,

    /// Log level (trace, debug, info, warn, error). Overridden by `RUST_LOG`.
    #[arg(long, default_value = "info", global = true)]
    log_level: String,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Interactive wizard that detects the local DAC and writes a starter
    /// `bridge.yml`. Run as the user that should own the file (commonly
    /// root, since the default target is /etc/rpi-camilla-bridge/).
    Init {
        /// Where to write the generated config. If omitted, the wizard
        /// asks interactively.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Quick health check — verifies snd-aloop, CamillaDSP, configs and
    /// mDNS without changing any state. Exits non-zero on any failure.
    Doctor,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    init_logging(&cli.log_level);

    match cli.cmd {
        Some(Cmd::Init { output }) => return init::run_wizard(output),
        Some(Cmd::Doctor) => {
            return doctor::run(doctor::DoctorOpts {
                bridge_config: cli.bridge_config.as_deref(),
                idle_config: cli.idle_config.as_deref(),
                camilla_host: &cli.camilla_host,
                camilla_port: cli.camilla_port,
                device: &cli.device,
            });
        }
        None => {}
    }

    // Hard speaker-protection guard: refuse to open anything that isn't an
    // ALSA loopback. The DAC must always be reached through CamillaDSP, never
    // directly — bypassing CamillaDSP would skip every limiter and crossover
    // and could destroy the loudspeakers on a single missed click.
    enforce_loopback_only(&cli.device)?;

    let stop = Arc::new(AtomicBool::new(false));
    {
        let s = Arc::clone(&stop);
        ctrlc::set_handler(move || {
            info!("signal received, shutting down");
            s.store(true, Ordering::SeqCst);
        })
        .context("installing ctrl-c handler")?;
    }

    let listener = TcpListener::bind(("0.0.0.0", cli.port))
        .with_context(|| format!("binding TCP 0.0.0.0:{}", cli.port))?;
    info!(port = cli.port, "listening");

    // Best-effort: publish ourselves on mDNS so PCs can find us without
    // `--host`. Held until exit (Drop unregisters + shuts the daemon).
    let _mdns = match discovery::ServiceRegistration::try_register(cli.port) {
        Ok(reg) => Some(reg),
        Err(e) => {
            warn!(error = %e, "mDNS registration failed; clients will need --host");
            None
        }
    };

    accept_loop(&listener, &cli, &stop)?;
    info!("exit");
    Ok(())
}

/// Speaker-protection guard. snd-aloop's card name is hard-coded as
/// "Loopback" (configurable to a different *id* via modprobe — but the user
/// would still need the same id on both ends), so a substring check on the
/// device string is a reliable, fail-closed way to ensure the bridge never
/// writes to anything other than the loopback that feeds CamillaDSP.
///
/// Rejects:
///   - DAC card names: `hifiberry`, `dac`
///   - ALSA aliases that route anywhere: `default`, `null`, `sysdefault`,
///     `pulse`, `pipewire`, anything starting with `plug` (would silently
///     resample)
///   - Any device string that does not include the substring "loopback"
fn enforce_loopback_only(device: &str) -> Result<()> {
    let lower = device.to_ascii_lowercase();
    let banned_substrings = [
        "hifiberry",
        "dac",
        "sndrpi",
        "default",
        "null",
        "sysdefault",
        "pulse",
        "pipewire",
    ];
    for needle in banned_substrings {
        if lower.contains(needle) {
            return Err(anyhow::anyhow!(
                "refusing to open `{device}`: matches banned pattern `{needle}`. \
                 The bridge only writes to ALSA loopback devices so audio \
                 always passes through CamillaDSP (limiters/crossovers).",
            ));
        }
    }
    if lower.starts_with("plug") || lower.starts_with("plughw") {
        return Err(anyhow::anyhow!(
            "refusing to open `{device}`: `plug*` devices silently insert ALSA \
             format conversion that bypasses our format negotiation. Use a raw \
             `hw:Loopback,0,0` device instead.",
        ));
    }
    if !lower.contains("loopback") {
        return Err(anyhow::anyhow!(
            "refusing to open `{device}`: device string must contain \
             `loopback` (case-insensitive). The bridge is hard-locked to ALSA \
             loopback so audio always passes through CamillaDSP. If you need \
             to debug, edit pi-receiver's source — there is no CLI escape \
             hatch on purpose.",
        ));
    }
    Ok(())
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

fn accept_loop(listener: &TcpListener, cli: &Cli, stop: &Arc<AtomicBool>) -> Result<()> {
    listener
        .set_nonblocking(true)
        .context("setting non-blocking accept")?;

    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, peer)) => {
                info!(peer = %peer, "accepted connection");
                if let Err(e) = handle_session(stream, cli, stop) {
                    warn!(error = %e, "session ended with error");
                }
                if !stop.load(Ordering::SeqCst) {
                    info!("ready for next connection");
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock) => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                warn!(error = %e, "accept error");
                std::thread::sleep(Duration::from_millis(200));
            }
        }
    }
    Ok(())
}

fn handle_session(mut stream: TcpStream, cli: &Cli, stop: &Arc<AtomicBool>) -> Result<()> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .context("set_read_timeout")?;
    stream.set_nodelay(true).ok();

    let header = Header::read_from(&mut stream).context("reading wire header")?;
    info!(
        format = ?header.format,
        channels = header.channels,
        sample_rate = header.sample_rate,
        "header received",
    );

    // Switch CamillaDSP to the bridge config so its capture matches what we send.
    if let Some(path) = &cli.bridge_config
        && !cli.camilla_host.is_empty()
    {
        camilla_ws::try_switch(&cli.camilla_host, cli.camilla_port, path, "bridge");
        // Brief settle time for CamillaDSP to re-open ALSA capture before we push.
        std::thread::sleep(Duration::from_millis(500));
    }

    let session = run_session(stream, &header, cli, stop);

    if let Some(path) = &cli.idle_config
        && !cli.camilla_host.is_empty()
    {
        camilla_ws::try_switch(&cli.camilla_host, cli.camilla_port, path, "idle");
    }

    session
}

fn run_session(
    stream: TcpStream,
    header: &Header,
    cli: &Cli,
    stop: &Arc<AtomicBool>,
) -> Result<()> {
    let alsa = alsa_out::AlsaPlayback::open(&cli.device, header, cli.buffer_ms)
        .context("opening ALSA playback")?;

    // 32 chunks × ~64 KiB worst case = ~2 MiB cap. Plenty of slack across any
    // sane buffer_ms; below that, TCP back-pressure will throttle the sender.
    let (tx, rx) = bounded::<Vec<u8>>(32);

    let alsa_handle = std::thread::Builder::new()
        .name("alsa-writer".into())
        .spawn(move || alsa.run(rx))
        .context("spawning ALSA writer")?;

    // Clone the TCP stream so this thread can shutdown(Both) on it when a
    // global stop is requested. Without that, the net-reader thread sits
    // blocked in `read()` waiting for the 5 s read-timeout, and our
    // `net_handle.join()` below sits blocked behind it — `systemctl stop`
    // hangs for over a minute before SIGKILL takes over.
    //
    // Same pattern as pc-sender::main::try_session.
    let stream_for_shutdown = stream
        .try_clone()
        .context("cloning TCP stream for shutdown signaling")?;

    let net_handle = std::thread::Builder::new()
        .name("net-reader".into())
        .spawn(move || net_in::pump(stream, tx))
        .context("spawning net reader")?;

    while !net_handle.is_finished() {
        if stop.load(Ordering::SeqCst) {
            // Force the in-flight `read()` to return immediately so the
            // thread can observe the broken connection and exit. The OS
            // closes the socket regardless once the process exits, but
            // doing it here lets the join below complete in <100 ms.
            let _ = stream_for_shutdown.shutdown(std::net::Shutdown::Both);
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    let net_total = net_handle
        .join()
        .map_err(|_| anyhow::anyhow!("net thread panicked"))?
        .context("net thread errored")?;
    let alsa_res = alsa_handle
        .join()
        .map_err(|_| anyhow::anyhow!("alsa thread panicked"))?;
    info!(bytes_received = net_total, "session ended");
    alsa_res
}

#[cfg(test)]
mod tests {
    use super::enforce_loopback_only;

    #[test]
    fn allows_canonical_loopback_devices() {
        for ok in [
            "hw:Loopback,0,0",
            "hw:Loopback,0",
            "hw:CARD=Loopback,DEV=0",
            "hw:CARD=Loopback,DEV=0,SUBDEV=0",
            "loopback",
        ] {
            assert!(enforce_loopback_only(ok).is_ok(), "should allow {ok}");
        }
    }

    #[test]
    fn rejects_dac_and_aliases() {
        for bad in [
            "hw:CARD=sndrpihifiberry,DEV=0", // direct DAC
            "hw:sndrpihifiberry,0",
            "hw:dac,0,0",
            "default",
            "sysdefault:CARD=sndrpihifiberry",
            "null",
            "pulse",
            "pipewire",
            "plughw:Loopback,0,0", // plug layer hides format mismatches
            "plug:default",
            "hw:1,0,0", // numeric — could be the DAC
        ] {
            assert!(
                enforce_loopback_only(bad).is_err(),
                "must reject `{bad}` to keep the DAC unreachable"
            );
        }
    }
}
