//! Audio-activity detector used for "smart sleep" — keep the virtual
//! output visible in the OS Sound settings always, but only hold the
//! TCP/loopback path open while audio is actually flowing.
//!
//! The cpal callback notes peak amplitude per chunk; the main thread
//! polls `is_active()` to decide whether to keep a TCP session running
//! or release it so other CamillaDSP sources (Tidal, Roon, mpd) can
//! grab the loopback. Transparent to the user — no flicker in Sound
//! settings, just the Pi's bridge engaging when there's audio to send
//! and stepping aside when there isn't.

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Threshold below which a chunk counts as "silence". -60 dBFS is the
/// usual line; well below normal music dynamic range, well above the
/// noise floor of a quiet idle PA stream.
const SILENCE_DBFS: f32 = -60.0;

#[derive(Clone)]
pub struct ActivityTracker {
    /// Milliseconds-since-epoch of the last non-silent chunk. The
    /// audio callback writes; the main thread reads. Atomic, lock-free.
    last_active_ms: Arc<AtomicI64>,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self {
            last_active_ms: Arc::new(AtomicI64::new(now_ms())),
        }
    }

    /// Called from the cpal callback (real-time-safe).
    /// `peak_abs` is the maximum absolute sample magnitude in the chunk,
    /// expressed as a value in [0.0, 1.0] (full-scale = 1.0).
    pub fn note_peak(&self, peak_abs: f32) {
        if peak_abs <= 0.0 {
            return;
        }
        let dbfs = 20.0 * peak_abs.log10();
        if dbfs > SILENCE_DBFS {
            self.last_active_ms.store(now_ms(), Ordering::Relaxed);
        }
    }

    /// True if a non-silent chunk arrived within `idle_after`.
    pub fn is_active(&self, idle_after: Duration) -> bool {
        let last = self.last_active_ms.load(Ordering::Relaxed);
        now_ms().saturating_sub(last) < idle_after.as_millis() as i64
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}
