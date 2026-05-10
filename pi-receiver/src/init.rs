//! Interactive `bridge.yml` wizard.
//!
//! Goal: someone running `sudo pi-receiver init` answers three or four
//! questions and walks away with a valid bridge config that matches the
//! DAC actually on the box. They don't need to know YAML, ALSA card
//! naming conventions, or the wire-protocol contract — the wizard
//! enumerates ALSA cards via `/proc/asound/cards`, drops the loopback
//! out of the list (writing into your own loopback is the one thing
//! the bridge must never do), and asks the user to pick.
//!
//! Output is the `deploy/bridge.yml.example` template, embedded at
//! compile time and substituted in three spots: the playback device's
//! `hw:CARD=` line, the channel count, and the capture/playback
//! samplerate. Everything else (filters, mixers, pipeline) is left as
//! the empty-pipeline passthrough that the example ships with — users
//! add their own protections from there.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};

const TEMPLATE: &str = include_str!("../../deploy/bridge.yml.example");

#[derive(Debug, Clone)]
struct Card {
    /// Numeric card index (0, 1, 2, …) — what `hw:N,…` uses.
    index: u32,
    /// Card id (`Loopback`, `sndrpihifiberry`, …) — what `hw:CARD=` uses.
    id: String,
    /// Human-readable description from `/proc/asound/cards`.
    name: String,
}

pub fn run_wizard(output: Option<PathBuf>) -> Result<()> {
    println!("rpi-camilla-bridge: bridge.yml wizard");
    println!("press Ctrl+C at any prompt to abort\n");

    let cards = read_cards().context("listing /proc/asound/cards")?;
    let candidates: Vec<&Card> = cards
        .iter()
        // Loopback is always card 0 in the standard install; in any case,
        // never let the wizard suggest writing into the same card we
        // capture from — that's the one configuration that bypasses the
        // bridge's protection guard.
        .filter(|c| !c.id.eq_ignore_ascii_case("Loopback"))
        .collect();
    if candidates.is_empty() {
        bail!(
            "no DAC cards found in /proc/asound/cards (only `Loopback`); \
             did you load your DAC kernel module?"
        );
    }

    println!("Available output cards:");
    for (i, c) in candidates.iter().enumerate() {
        println!("  [{i}] {:24}  {}", c.id, c.name);
    }
    let choice = prompt_index("Pick a card by number", candidates.len())?;
    let card = candidates[choice];

    let channels: u16 = prompt_with_default(
        "Channels — 2 for stereo, 8 for an 8-channel DAC like the StudioDAC8x",
        "2",
    )?
    .parse()
    .context("invalid channel count")?;
    if channels == 0 {
        bail!("channel count must be > 0");
    }

    let rate: u32 = prompt_with_default(
        "Sample rate in Hz — 48000 matches the OS default on Windows / Linux",
        "48000",
    )?
    .parse()
    .context("invalid sample rate")?;
    if rate == 0 {
        bail!("sample rate must be > 0");
    }

    let yaml = render(card, channels, rate);

    println!("\n----- generated bridge.yml -----");
    println!("{yaml}");
    println!("----- (preview) -----\n");

    let target = match output {
        Some(p) => p,
        None => {
            let raw = prompt_with_default("Write to", "/etc/rpi-camilla-bridge/bridge.yml")?;
            PathBuf::from(raw)
        }
    };

    if target.exists()
        && !prompt_yes(
            &format!("{} already exists — overwrite?", target.display()),
            false,
        )?
    {
        println!("Aborted; no file changed.");
        return Ok(());
    }
    if let Some(parent) = target.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        bail!(
            "parent directory {} doesn't exist — create it first \
             (`sudo install -d {}`) and re-run",
            parent.display(),
            parent.display()
        );
    }
    fs::write(&target, &yaml).with_context(|| format!("writing {}", target.display()))?;

    println!("✓ wrote {} bytes to {}", yaml.len(), target.display());
    println!();
    println!("Next steps:");
    println!("  1. systemctl restart pi-receiver");
    println!("  2. On a PC on the same LAN, run `pc-sender` (no flags) — it'll");
    println!("     auto-discover this Pi over mDNS.");
    Ok(())
}

fn read_cards() -> Result<Vec<Card>> {
    let raw = fs::read_to_string("/proc/asound/cards").context("reading /proc/asound/cards")?;
    let mut out = Vec::new();
    for chunk in raw.lines().collect::<Vec<_>>().chunks(2) {
        // /proc/asound/cards prints two lines per card:
        //   "  0 [Loopback       ]: Loopback - Loopback"
        //   "                       Loopback 1"
        let header = chunk.first().copied().unwrap_or("");
        let body = chunk.get(1).copied().unwrap_or("").trim();
        if let Some(card) = parse_card(header, body) {
            out.push(card);
        }
    }
    Ok(out)
}

fn parse_card(header: &str, body: &str) -> Option<Card> {
    let header = header.trim_start();
    let (idx_str, rest) = header.split_once(' ')?;
    let index: u32 = idx_str.parse().ok()?;
    let id_start = rest.find('[')? + 1;
    let id_end = rest[id_start..].find(']')? + id_start;
    let id = rest[id_start..id_end].trim().to_string();
    let after_bracket = rest
        .get(id_end + 1..)
        .unwrap_or("")
        .trim_start_matches(": ");
    let name = if body.is_empty() {
        after_bracket.to_string()
    } else {
        format!("{after_bracket} ({body})")
    };
    Some(Card { index, id, name })
}

fn render(card: &Card, channels: u16, rate: u32) -> String {
    let mut yaml = TEMPLATE.to_string();
    yaml = yaml.replace(
        "device: hw:CARD=YourDAC,DEV=0   # ← change to your DAC card name",
        &format!(
            "device: hw:CARD={},DEV=0   # auto-detected: card {} ({})",
            card.id, card.index, card.name
        ),
    );
    // The template has `channels: 2` twice (capture and playback) and one
    // `capture_samplerate: 48000`. We only mutate the playback channel
    // count via a targeted swap; capture stays at 2 to match what cpal
    // produces by default.
    let target = format!("channels: {channels}");
    yaml = yaml.replacen(
        "channels: 2\n  device: hw:CARD=",
        &format!("{target}\n  device: hw:CARD="),
        1,
    );
    yaml = yaml.replace(
        "capture_samplerate: 48000",
        &format!("capture_samplerate: {rate}"),
    );
    yaml = yaml.replace(
        "samplerate: 48000           # raise to your DAC's preferred rate (96000, etc.)",
        &format!("samplerate: {rate}           # auto-set by `pi-receiver init`"),
    );
    yaml
}

fn prompt_with_default(question: &str, default: &str) -> Result<String> {
    print!("{question} [{default}]: ");
    io::stdout().flush().ok();
    let mut line = String::new();
    let stdin = io::stdin();
    stdin
        .lock()
        .read_line(&mut line)
        .context("reading from stdin")?;
    let trimmed = line.trim();
    if trimmed.is_empty() {
        Ok(default.to_string())
    } else {
        Ok(trimmed.to_string())
    }
}

fn prompt_index(question: &str, max: usize) -> Result<usize> {
    let raw = prompt_with_default(question, "0")?;
    let n: usize = raw.parse().context("not a number")?;
    if n >= max {
        return Err(anyhow!(
            "choice {n} is out of range (have {max} candidates, indices 0..{})",
            max - 1
        ));
    }
    Ok(n)
}

fn prompt_yes(question: &str, default: bool) -> Result<bool> {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    print!("{question} {suffix}: ");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin().lock().read_line(&mut line)?;
    let answer = line.trim().to_lowercase();
    Ok(match answer.as_str() {
        "" => default,
        "y" | "yes" | "s" | "sim" => true,
        "n" | "no" | "não" | "nao" => false,
        _ => default,
    })
}

#[allow(dead_code)]
fn _ensure_path_under(_p: &Path) {}
