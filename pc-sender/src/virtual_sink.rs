//! Linux-only: register `rpi-camilla-bridge` as a PulseAudio / PipeWire
//! virtual output so it appears in the OS Sound settings as a regular
//! speaker.
//!
//! Mechanism: shell out to `pactl` to load `module-null-sink` (creates
//! a sink with no real hardware), then set the sink's monitor as the
//! system default source. After that, any application routed to
//! "rpi-camilla-bridge" produces audio that cpal's default input
//! captures — and the bridge ships it to the Pi.
//!
//! The previous default source is captured on `create` and restored
//! on `Drop`; the loaded module is unloaded on `Drop` as well. Drop
//! also fires from the ctrl-c handler path (the receiver is stored in
//! `main`'s scope and dropped on graceful exit).

use std::process::Command;

use anyhow::{Context, Result, anyhow};
use tracing::{info, warn};

const SINK_NAME: &str = "rpi_camilla_bridge";

pub struct VirtualSink {
    module_id: String,
    previous_default_source: Option<String>,
}

impl VirtualSink {
    /// Create the null-sink and route the system default source at it.
    /// Returns `Ok(None)` (with a warning logged) if `pactl` is unavailable
    /// — caller should then fall back to whatever cpal exposes as default.
    /// `description` is the user-facing label shown in the OS Sound
    /// settings; it can contain spaces and parentheses but quote-marks
    /// inside it are stripped to keep the pactl argument well-formed.
    pub fn try_create(description: &str) -> Result<Option<Self>> {
        if which_pactl().is_none() {
            warn!(
                "pactl not found on PATH; skipping virtual sink. \
                 The bridge will capture cpal's default input device — \
                 likely your microphone. Pass `--device <name>` to pick a \
                 specific source, or install PulseAudio / PipeWire utils."
            );
            return Ok(None);
        }

        let prev = run_pactl(&["get-default-source"])
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());

        // Load the null-sink. `pactl load-module` prints the module ID on
        // success; a non-zero exit means the load failed (already loaded
        // with the same name, sink_properties syntax issue, etc.).
        //
        // pactl's sink_properties parser splits the value on regular spaces
        // (treating each space as a key=value separator), so a description
        // like `"Raspberry Pi (hifiberry)"` would be truncated to
        // `Raspberry`. We swap regular spaces for U+00A0 (non-breaking
        // space) — it's visually identical in every audio settings GUI we
        // tested and survives pactl's parser.
        let safe_desc = description.replace('"', "").replace(' ', "\u{00a0}");
        let load_args = [
            "load-module",
            "module-null-sink",
            &format!("sink_name={SINK_NAME}"),
            &format!("sink_properties=device.description={safe_desc}"),
        ];
        let module_id = run_pactl(&load_args)
            .context("loading null-sink module via pactl")?
            .trim()
            .to_string();

        if module_id.is_empty() || !module_id.chars().all(|c| c.is_ascii_digit()) {
            return Err(anyhow!(
                "pactl returned an unexpected module id: {module_id:?}"
            ));
        }

        info!(
            module_id = %module_id,
            sink = SINK_NAME,
            description = %safe_desc,
            "registered virtual output (visible in Settings → Sound → Output)",
        );

        // Route the system default source to our monitor so cpal default
        // input captures from this sink.
        let monitor = format!("{SINK_NAME}.monitor");
        if let Err(e) = run_pactl(&["set-default-source", &monitor]) {
            warn!(error = %e, "failed to make virtual sink the default source; capture may pick up the previous source");
        }

        Ok(Some(Self {
            module_id,
            previous_default_source: prev,
        }))
    }
}

impl Drop for VirtualSink {
    fn drop(&mut self) {
        if let Some(prev) = &self.previous_default_source
            && let Err(e) = run_pactl(&["set-default-source", prev])
        {
            warn!(error = %e, source = %prev, "failed to restore previous default source");
        }
        if let Err(e) = run_pactl(&["unload-module", &self.module_id]) {
            warn!(error = %e, module = %self.module_id, "failed to unload virtual sink module — `pactl unload-module {}` to clean up manually", self.module_id);
        } else {
            info!(module_id = %self.module_id, "virtual sink unloaded");
        }
    }
}

fn run_pactl(args: &[&str]) -> Result<String> {
    let output = Command::new("pactl")
        .args(args)
        .output()
        .context("spawning pactl")?;
    if !output.status.success() {
        return Err(anyhow!(
            "pactl {} exited {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn which_pactl() -> Option<String> {
    Command::new("pactl")
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|_| "pactl".into())
}
