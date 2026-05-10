//! Linux-only: keep CamillaDSP's main volume in sync with whatever the
//! user picked for the `rpi_camilla_bridge` sink in their OS Sound
//! settings.
//!
//! Subscribes to `pactl subscribe`, watches for `change' on sink`
//! events, queries the named sink's volume, and pushes the value to
//! CamillaDSP via the same websocket path the receiver uses.
//!
//! Tolerant: if `pactl` isn't installed, or our sink doesn't exist
//! yet, or the WS connect fails, we log at debug and keep running.
//! Volume sync is a quality-of-life feature; it must never bring down
//! the audio path.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::json;
use tracing::{debug, warn};
use tungstenite::Message;

const SINK_NAME: &str = "rpi_camilla_bridge";

pub fn spawn(stop: Arc<AtomicBool>, camilla_host: String, camilla_port: u16) {
    let _ = thread::Builder::new()
        .name("volume-sync".into())
        .spawn(move || run(stop, camilla_host, camilla_port));
}

fn run(stop: Arc<AtomicBool>, host: String, port: u16) {
    if !pactl_available() {
        debug!("pactl not on PATH; volume sync disabled");
        return;
    }

    // Push the current sink volume on startup so Camilla starts where
    // PA already shows; otherwise the first volume change you make is
    // the only thing it sees, and the gap before that is silent-confusing.
    if let Some(db) = read_sink_db() {
        push_volume(&host, port, db);
    }

    while !stop.load(Ordering::SeqCst) {
        if let Err(e) = run_subscribe_loop(&stop, &host, port) {
            warn!(error = %e, "pactl subscribe loop exited; retry in 1 s");
            thread::sleep(Duration::from_secs(1));
        }
    }
}

fn run_subscribe_loop(stop: &Arc<AtomicBool>, host: &str, port: u16) -> Result<()> {
    // Force English output so our `change' on sink` filter survives every
    // locale; the user's LC_* may translate the "Event 'change'" prefix.
    let mut child = Command::new("pactl")
        .env("LC_ALL", "C")
        .arg("subscribe")
        .stdout(Stdio::piped())
        .spawn()
        .context("spawning `pactl subscribe`")?;

    let stdout = child.stdout.take().context("pactl stdout missing")?;
    let reader = BufReader::new(stdout);

    for line in reader.lines() {
        if stop.load(Ordering::SeqCst) {
            let _ = child.kill();
            return Ok(());
        }
        let Ok(line) = line else { continue };
        // pactl emits one event per line, e.g.
        //   Event 'change' on sink #859
        // We trigger on every sink change and let the volume query
        // narrow down to ours by name; cheaper than parsing the index
        // out and mapping it back to a name.
        if line.contains("change' on sink") {
            if let Some(db) = read_sink_db() {
                push_volume(host, port, db);
            }
        }
    }
    let _ = child.wait();
    Ok(())
}

fn read_sink_db() -> Option<f64> {
    // Same locale clamp as the subscribe call — get-sink-volume emits
    // numbers in `0,00` (comma decimal separator) under pt_BR / fr_FR
    // unless we pin C, and `f64::parse` doesn't grok comma decimals.
    let output = Command::new("pactl")
        .env("LC_ALL", "C")
        .args(["get-sink-volume", SINK_NAME])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_db(&String::from_utf8_lossy(&output.stdout))
}

/// Parse one or more `<n> dB` tokens out of pactl's `get-sink-volume`
/// output and average them. The output is a single line such as
/// `Volume: front-left: 65535 / 100% / 0.00 dB,   front-right: 65535 / 100% / 0.00 dB`.
fn parse_db(text: &str) -> Option<f64> {
    let mut samples = Vec::new();
    let words: Vec<&str> = text.split_whitespace().collect();
    for pair in words.windows(2) {
        if pair[1].starts_with("dB") {
            let cleaned = pair[0].trim_end_matches(',');
            if let Ok(v) = cleaned.parse::<f64>() {
                samples.push(v);
            }
        }
    }
    if samples.is_empty() {
        return None;
    }
    Some(samples.iter().sum::<f64>() / samples.len() as f64)
}

fn push_volume(host: &str, port: u16, db: f64) {
    // Clamp to a sane range. 0 dB is unity (PA at 100%); below -100 dB
    // is effectively muted. CamillaDSP's `volume_limit` in bridge.yml
    // caps further, so anything we send is already attenuated by it.
    let clamped = db.clamp(-100.0, 0.0);

    let url = format!("ws://{host}:{port}");
    let (mut socket, _resp) = match tungstenite::connect(&url) {
        Ok(p) => p,
        Err(e) => {
            debug!(error = %e, %url, "WS connect for SetVolume failed");
            return;
        }
    };

    let wire = match serde_json::to_string(&json!({ "SetVolume": clamped })) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "SetVolume serialize");
            return;
        }
    };

    if let Err(e) = socket.send(Message::Text(wire.into())) {
        debug!(error = %e, "SetVolume send failed");
        return;
    }
    // We don't really need the response, but reading it lets the WS
    // shut down cleanly without RST-ing the server.
    let _ = socket.read();
    let _ = socket.close(None);
    debug!(volume_db = clamped, "volume synced");
}

fn pactl_available() -> bool {
    Command::new("pactl")
        .arg("--version")
        .output()
        .ok()
        .is_some_and(|o| o.status.success())
}
