//! Friendly status output for end users.
//!
//! pc-sender's default mode is "quiet": only `WARN`+ tracing reaches the
//! terminal, while these helpers print one human-readable line per major
//! state transition. Verbose mode (`--verbose` or `--log-level=…`) keeps
//! the structured tracing output instead, which is what we want bug
//! reports to capture.
//!
//! These print to stdout so a user can pipe them somewhere; warnings
//! and errors stay on stderr through tracing.

use std::io::{self, IsTerminal, Write};

fn supports_color() -> bool {
    io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn fmt(prefix: &str, color: &str, msg: &str) -> String {
    if supports_color() {
        format!("\x1b[{color}m{prefix}\x1b[0m {msg}")
    } else {
        format!("{prefix} {msg}")
    }
}

pub fn ok(msg: impl AsRef<str>) {
    let _ = writeln!(io::stdout(), "{}", fmt("✓", "1;32", msg.as_ref()));
}

pub fn step(msg: impl AsRef<str>) {
    let _ = writeln!(io::stdout(), "{}", fmt("▸", "1;34", msg.as_ref()));
}

pub fn info(msg: impl AsRef<str>) {
    let _ = writeln!(io::stdout(), "{}", fmt("·", "1;36", msg.as_ref()));
}

pub fn warn(msg: impl AsRef<str>) {
    let _ = writeln!(io::stderr(), "{}", fmt("!", "1;33", msg.as_ref()));
}
