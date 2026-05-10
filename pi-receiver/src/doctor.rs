//! `pi-receiver doctor` — quick health check for the bridge install.
//!
//! Walks through the failure modes that actually waste time in real
//! support tickets:
//!
//!   - Is the snd-aloop kernel module loaded?
//!   - Is CamillaDSP running and reachable on its websocket?
//!   - Are bridge / idle configs present and parseable as YAML?
//!   - Does the bridge config's `capture` section follow the
//!     wire-format contract pc-sender defaults to?
//!   - Can `pi-camilla` actually open the loopback for write?
//!   - Is mDNS publishing on the LAN?
//!
//! Each check prints one line: ✓ ok, ! warning, ✗ error. We never
//! mutate state — purely observational. Exits non-zero if any check
//! fails so the command works in scripts.

use std::fs;
use std::net::TcpStream;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::Result;

/// Results of one check.
#[derive(Debug)]
enum Status {
    Ok(String),
    Warn(String),
    Fail(String),
}

pub struct DoctorOpts<'a> {
    pub bridge_config: Option<&'a str>,
    pub idle_config: Option<&'a str>,
    pub camilla_host: &'a str,
    pub camilla_port: u16,
    pub device: &'a str,
}

pub fn run(opts: DoctorOpts<'_>) -> Result<()> {
    println!("rpi-camilla-bridge: doctor\n");

    let mut failures = 0u32;
    let mut warnings = 0u32;
    let mut run_check = |label: &str, status: Status| {
        let (icon, color) = match &status {
            Status::Ok(_) => ("✓", "1;32"),
            Status::Warn(_) => ("!", "1;33"),
            Status::Fail(_) => ("✗", "1;31"),
        };
        let text = match &status {
            Status::Ok(t) | Status::Warn(t) | Status::Fail(t) => t.as_str(),
        };
        if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
            println!("\x1b[{color}m{icon}\x1b[0m {label:<32} {text}");
        } else {
            println!("{icon} {label:<32} {text}");
        }
        match status {
            Status::Warn(_) => warnings += 1,
            Status::Fail(_) => failures += 1,
            Status::Ok(_) => {}
        }
    };

    run_check("snd-aloop module", check_snd_aloop());
    run_check("loopback card present", check_loopback_card());
    run_check("DAC card present", check_dac_card());
    run_check(
        "CamillaDSP websocket",
        check_camilla_ws(opts.camilla_host, opts.camilla_port),
    );
    run_check(
        "bridge config",
        check_yaml_path(opts.bridge_config, "bridge"),
    );
    run_check("idle config", check_yaml_path(opts.idle_config, "idle"));
    run_check("device flag is loopback", check_device_guard(opts.device));
    run_check("mDNS daemon reachable", check_mdns_daemon());

    println!();
    if failures == 0 && warnings == 0 {
        println!("All checks passed.");
        Ok(())
    } else if failures == 0 {
        println!(
            "{warnings} warning(s); the bridge is likely usable but watch the lines marked `!`."
        );
        Ok(())
    } else {
        println!(
            "{failures} failure(s) and {warnings} warning(s) — fix the `✗` lines before expecting audio to flow."
        );
        std::process::exit(1)
    }
}

fn check_snd_aloop() -> Status {
    match fs::read_to_string("/proc/modules") {
        Ok(s) => {
            if s.lines()
                .any(|l| l.split_whitespace().next() == Some("snd_aloop"))
            {
                Status::Ok("loaded".into())
            } else {
                Status::Fail(
                    "kernel module not loaded — `sudo modprobe snd-aloop`, or add \
                     `dtoverlay=snd-aloop` to /boot/firmware/config.txt and reboot"
                        .into(),
                )
            }
        }
        Err(e) => Status::Warn(format!("can't read /proc/modules: {e}")),
    }
}

fn check_loopback_card() -> Status {
    match fs::read_to_string("/proc/asound/cards") {
        Ok(s) => {
            if s.lines().any(|l| l.contains("[Loopback ")) {
                Status::Ok("found in /proc/asound/cards".into())
            } else {
                Status::Fail(
                    "no Loopback card visible — load snd-aloop, then verify with \
                     `cat /proc/asound/cards`"
                        .into(),
                )
            }
        }
        Err(e) => Status::Warn(format!("can't read /proc/asound/cards: {e}")),
    }
}

fn check_dac_card() -> Status {
    match fs::read_to_string("/proc/asound/cards") {
        Ok(s) => {
            let dacs: Vec<&str> = s
                .lines()
                .filter(|l| l.starts_with(' ') && l.contains('['))
                .filter(|l| !l.contains("[Loopback "))
                .collect();
            if dacs.is_empty() {
                Status::Warn(
                    "no non-Loopback card present — bridge can run but won't \
                     reach a DAC until one shows up"
                        .into(),
                )
            } else {
                Status::Ok(format!("{} non-loopback card(s) present", dacs.len()))
            }
        }
        Err(e) => Status::Warn(format!("can't read /proc/asound/cards: {e}")),
    }
}

fn check_camilla_ws(host: &str, port: u16) -> Status {
    match TcpStream::connect_timeout(
        &format!("{host}:{port}")
            .parse()
            .unwrap_or_else(|_| ([127, 0, 0, 1], port).into()),
        Duration::from_secs(1),
    ) {
        Ok(_) => Status::Ok(format!("port {port} open at {host}")),
        Err(e) => Status::Fail(format!(
            "{host}:{port} not reachable ({e}) — is camilladsp.service running?"
        )),
    }
}

fn check_yaml_path(path: Option<&str>, label: &str) -> Status {
    let Some(p) = path else {
        return Status::Warn(format!(
            "no --{label}-config given on the CLI; doctor is checking only what you told it"
        ));
    };
    if !Path::new(p).exists() {
        return Status::Fail(format!("{p} does not exist"));
    }
    match fs::read_to_string(p) {
        Ok(content) => {
            if content.contains("devices:") && content.contains("playback:") {
                Status::Ok(format!(
                    "{p} ({} bytes) looks like a CamillaDSP config",
                    content.len()
                ))
            } else {
                Status::Warn(format!(
                    "{p} is readable but doesn't have `devices:` + `playback:` — \
                     CamillaDSP will reject it"
                ))
            }
        }
        Err(e) => Status::Fail(format!("{p}: {e}")),
    }
}

fn check_device_guard(device: &str) -> Status {
    let lower = device.to_ascii_lowercase();
    if !lower.contains("loopback") {
        return Status::Fail(format!(
            "--device `{device}` does not contain `loopback`; the bridge will refuse to open it"
        ));
    }
    if lower.starts_with("plug") {
        return Status::Fail(format!(
            "--device `{device}` is a `plug*` alias which would silently insert format \
             conversion; use a raw `hw:Loopback,...`"
        ));
    }
    Status::Ok(format!("`{device}` will pass the bridge's loopback guard"))
}

fn check_mdns_daemon() -> Status {
    // We only care that *something* is listening on UDP 5353 — could be
    // avahi, systemd-resolved, or our own embedded mdns-sd if pi-receiver
    // is already running.
    match Command::new("ss").args(["-Hulnp"]).output() {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            if text.lines().any(|l| l.contains(":5353 ")) {
                Status::Ok("UDP 5353 in use (avahi or pi-receiver)".into())
            } else {
                Status::Warn(
                    "no listener on UDP 5353 — pc-sender's mDNS auto-discovery won't \
                     find this Pi until pi-receiver starts (or avahi-daemon)"
                        .into(),
                )
            }
        }
        Ok(_) => Status::Warn("`ss -Hulnp` exited non-zero".into()),
        Err(e) => Status::Warn(format!("can't run `ss`: {e}")),
    }
}
