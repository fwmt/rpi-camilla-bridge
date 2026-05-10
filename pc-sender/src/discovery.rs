//! mDNS / DNS-SD client.
//!
//! Browses for `_camilla-bridge._tcp.local.` and returns whatever
//! Pi answers within a configurable timeout. If multiple answer, the
//! caller decides what to do — typically asking the user to re-run
//! with `--host`.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent};

const SERVICE_TYPE: &str = "_camilla-bridge._tcp.local.";

#[derive(Debug, Clone)]
pub struct DiscoveredPeer {
    pub fullname: String,
    /// The mDNS hostname (`hifiberry.local`). We hand this off to the OS
    /// resolver instead of picking one of the announced IPs ourselves —
    /// most Pis advertise multiple interfaces (eno1, docker bridges,
    /// libvirt, vpn) and arbitrarily picking from a `HashSet` lands on
    /// a non-LAN interface roughly half the time.
    pub host_label: String,
    pub port: u16,
}

/// Browse the LAN for `_camilla-bridge._tcp.local.` services. Returns
/// every distinct peer that answers within `timeout`. The browse stops
/// early once the first peer is resolved if `wait_for_more` is `false`,
/// keeping discovery latency snappy when only one Pi is expected.
pub fn browse(timeout: Duration, wait_for_more: bool) -> Result<Vec<DiscoveredPeer>> {
    let daemon = ServiceDaemon::new().context("starting mDNS browser")?;
    let receiver = daemon
        .browse(SERVICE_TYPE)
        .context("starting mDNS browse")?;

    let mut peers: HashMap<String, DiscoveredPeer> = HashMap::new();
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let chunk = remaining.min(Duration::from_millis(150));
        match receiver.recv_timeout(chunk) {
            Ok(ev) => {
                if let ServiceEvent::ServiceResolved(info) = ev {
                    let peer = DiscoveredPeer {
                        fullname: info.get_fullname().to_string(),
                        host_label: info.get_hostname().trim_end_matches('.').to_string(),
                        port: info.get_port(),
                    };
                    peers.insert(peer.fullname.clone(), peer);
                    if !wait_for_more {
                        break;
                    }
                }
            }
            Err(_) => {
                // Timeout on this chunk — loop back to check the deadline.
            }
        }
    }

    let _ = daemon.shutdown();
    Ok(peers.into_values().collect())
}
