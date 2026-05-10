//! TCP sender thread.
//!
//! Pulls byte chunks off the channel and writes them to the connected socket.
//! Returns when:
//!   - the audio side drops its sender (`Disconnected`),
//!   - a write to the socket fails (peer gone, network broken),
//!   - or the global `stop` flag is set (graceful shutdown).
//!
//! The shutdown path is short by design: `recv_timeout` polls the flag every
//! 100 ms so Ctrl+C in `pc-sender` returns within that window even if the
//! audio callback is in the middle of producing the next batch.

use std::io::Write;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError};
use tracing::{debug, warn};

const POLL_TICK: Duration = Duration::from_millis(100);

pub fn pump(mut stream: TcpStream, rx: Receiver<Vec<u8>>, stop: Arc<AtomicBool>) -> Result<u64> {
    let mut total: u64 = 0;
    while !stop.load(Ordering::SeqCst) {
        let chunk = match rx.recv_timeout(POLL_TICK) {
            Ok(c) => c,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                debug!("audio side closed channel; ending net pump");
                return Ok(total);
            }
        };
        if let Err(e) = stream.write_all(&chunk) {
            warn!(error = %e, "TCP write error; reconnect needed");
            return Err(e.into());
        }
        total += chunk.len() as u64;
    }
    debug!("stop signal observed; ending net pump");
    Ok(total)
}
