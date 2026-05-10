//! mDNS / DNS-SD service registration.
//!
//! Publishes `_camilla-bridge._tcp.local.` so `pc-sender` running on
//! any LAN client can discover the receiver without `--host`. Service
//! addresses are auto-refreshed by `mdns-sd` when local interfaces
//! come and go (Wi-Fi reconnects, Ethernet plug-ins).
//!
//! Tolerant: if the daemon refuses to start (no IPv4 multicast support,
//! locked-down environment), the receiver still serves on TCP — clients
//! can reach it the old way via `--host`.

use std::net::IpAddr;

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceInfo};
use tracing::{info, warn};

// DNS-SD limits the service-type label (including the leading underscore)
// to 15 bytes. `_camilla-bridge` is exactly 15 — descriptive and on-spec.
const SERVICE_TYPE: &str = "_camilla-bridge._tcp.local.";

pub struct ServiceRegistration {
    daemon: ServiceDaemon,
    fullname: String,
}

impl ServiceRegistration {
    pub fn try_register(port: u16) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;

        let host_raw = gethostname::gethostname()
            .into_string()
            .unwrap_or_else(|_| "rpi-camilla-bridge".to_string());
        let instance = host_raw.clone();
        let host_name = format!("{}.local.", host_raw.trim_end_matches(".local"));

        let no_addrs: Vec<IpAddr> = Vec::new();
        let info = ServiceInfo::new(
            SERVICE_TYPE,
            &instance,
            &host_name,
            &no_addrs[..],
            port,
            &[("version", env!("CARGO_PKG_VERSION")), ("proto", "1")][..],
        )
        .context("building ServiceInfo")?
        .enable_addr_auto();

        let fullname = info.get_fullname().to_string();
        daemon.register(info).context("registering mDNS service")?;
        info!(
            service = SERVICE_TYPE,
            instance = %instance,
            port,
            "mDNS service published",
        );

        Ok(Self { daemon, fullname })
    }
}

impl Drop for ServiceRegistration {
    fn drop(&mut self) {
        if let Err(e) = self.daemon.unregister(&self.fullname) {
            warn!(error = %e, "mDNS unregister failed");
        }
        if let Err(e) = self.daemon.shutdown() {
            warn!(error = %e, "mDNS daemon shutdown failed");
        }
    }
}
