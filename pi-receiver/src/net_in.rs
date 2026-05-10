//! TCP reader thread.
//!
//! Reads PCM bytes off a single connected socket in 64 KiB chunks and pushes
//! `Vec<u8>` onto the bounded channel that feeds the ALSA writer. Returns when
//! the peer closes the connection or any read errors out.

use std::io::Read;
use std::net::TcpStream;
use std::time::Duration;

use anyhow::Result;
use crossbeam_channel::{Sender, TrySendError};
use tracing::{debug, info, warn};

const READ_CHUNK: usize = 64 * 1024;

pub fn pump(mut stream: TcpStream, tx: Sender<Vec<u8>>) -> Result<u64> {
    // Read timeout makes shutdown responsive even if the peer goes silent.
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;

    let mut total: u64 = 0;
    let mut buf = vec![0u8; READ_CHUNK];

    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) => {
                info!(bytes_received = total, "peer closed connection");
                break;
            }
            Ok(n) => n,
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // No data within the read deadline — that's fine, just spin.
                continue;
            }
            Err(e) => {
                warn!(error = %e, "TCP read error → ending session");
                break;
            }
        };
        total += n as u64;

        let chunk = buf[..n].to_vec();
        match tx.try_send(chunk) {
            Ok(()) => {}
            Err(TrySendError::Full(c)) => {
                // Channel full means ALSA can't keep up — block instead of
                // dropping; TCP back-pressure naturally throttles the sender.
                if tx.send(c).is_err() {
                    debug!("alsa side hung up; ending net pump");
                    break;
                }
            }
            Err(TrySendError::Disconnected(_)) => {
                debug!("alsa side hung up; ending net pump");
                break;
            }
        }
    }

    Ok(total)
}
