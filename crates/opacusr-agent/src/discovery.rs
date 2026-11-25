//! Best-effort mDNS advertisement of this agent's signaling endpoint.
//!
//! Advertises `_opacusr._tcp.local.` (see `opacusr_proto::discovery`) so
//! viewers on the same network can find this host without typing a
//! server URL or an ID. The announcement is best-effort by design: any
//! error is logged and the agent runs on — a machine without multicast
//! (some VMs, docker default bridges) must not lose remote access over
//! it. The returned guard unregisters the service when dropped (RAII),
//! so `opacusr-agent run` stops announcing the moment it exits.
//!
//! The ID is not known at advertise time (the signaling server mints it
//! only after the first registration), so the agent starts announcing
//! without an `id` TXT record and re-announces with one as soon as the
//! primary endpoint reports its `Welcome` — [`AdvertiseGuard::set_id`].
//! Viewers use that record to resolve «connect by ID» straight onto
//! this host's endpoints (see `opacusr_proto::discovery::TXT_ID`).

use mdns_sd::{ServiceDaemon, ServiceInfo};
use opacusr_proto::discovery;

/// Guard keeping the mDNS advertisement alive; unregistering happens on
/// drop. Held by the agent service for the whole run, shared with the
/// endpoint loops (the ID arrives from the primary one).
pub struct AdvertiseGuard {
    daemon: ServiceDaemon,
    fqdn: String,
    instance: String,
    port: u16,
    lan_port: Option<u16>,
}

impl Drop for AdvertiseGuard {
    fn drop(&mut self) {
        // Best-effort: announce the removal and stop the daemon thread.
        // Receivers are dropped immediately — we do not wait for the
        // network to confirm during shutdown.
        if let Ok(rx) = self.daemon.unregister(&self.fqdn) {
            drop(rx);
        }
        if let Ok(rx) = self.daemon.shutdown() {
            drop(rx);
        }
    }
}

impl AdvertiseGuard {
    /// Re-announce the same service with the assigned ID added to the
    /// TXT records. Best-effort: a failure leaves the previous
    /// (ID-less) advertisement running — discovery still works, only
    /// the «connect by ID straight onto this host» resolution is lost.
    pub fn set_id(&self, id: opacusr_proto::PeerId) {
        let fqdn = self.fqdn.clone();
        if let Ok(rx) = self.daemon.unregister(&fqdn) {
            drop(rx);
        }
        let props = discovery::txt_properties("/signal", self.lan_port, Some(id));
        let svc = match ServiceInfo::new(
            discovery::SERVICE_TYPE,
            &self.instance,
            &format!("{}.local.", self.instance),
            "",
            self.port,
            &props[..],
        ) {
            Ok(svc) => svc.enable_addr_auto(),
            Err(e) => {
                tracing::debug!(target: "opacusr", "mDNS re-advertise with id: {e}");
                return;
            }
        };
        if let Err(e) = self.daemon.register(svc) {
            tracing::debug!(target: "opacusr", "mDNS re-advertise failed: {e}");
        } else {
            tracing::info!(target: "opacusr", "mDNS: advertising {fqdn} with id {id}");
        }
    }
}

/// Full service name for an instance: `<instance>.<SERVICE_TYPE>`.
fn service_fqdn(instance: &str) -> String {
    format!("{instance}.{}", discovery::SERVICE_TYPE)
}

/// Advertise the signaling endpoint on the local network.
///
/// * `name` — display name (sanitized into a dot-free instance label);
/// * `port` — the advertised signaling port;
/// * `lan_port` — the direct-LAN TCP port advertised in the `lan` TXT
///   record (the listener itself is served by the caller); `None`
///   omits the record (the `allow_lan` default);
/// * `peer_id` — the assigned ID when already known; the agent passes
///   `None` at startup and calls [`AdvertiseGuard::set_id`] once the
///   primary endpoint reports its ID.
///
/// Returns `None` when mDNS is unavailable (no multicast socket, no
/// daemon) — callers treat that as "discovery disabled", not an error.
pub fn advertise(
    name: &str,
    port: u16,
    lan_port: Option<u16>,
    peer_id: Option<opacusr_proto::PeerId>,
) -> Option<AdvertiseGuard> {
    let instance = discovery::sanitize_instance_name(name);
    let daemon = match ServiceDaemon::new() {
        Ok(d) => d,
        Err(e) => {
            tracing::debug!(target: "opacusr", "mDNS advertise unavailable: {e}");
            return None;
        }
    };
    let fqdn = service_fqdn(&instance);
    let props = discovery::txt_properties("/signal", lan_port, peer_id);
    // Empty IP string + enable_addr_auto(): announce on every interface
    // instead of pinning one address at registration time.
    let svc = match ServiceInfo::new(
        discovery::SERVICE_TYPE,
        &instance,
        &format!("{instance}.local."),
        "",
        port,
        &props[..],
    ) {
        Ok(svc) => svc.enable_addr_auto(),
        Err(e) => {
            tracing::debug!(target: "opacusr", "mDNS advertise: bad service info: {e}");
            return None;
        }
    };
    if let Err(e) = daemon.register(svc) {
        tracing::debug!(target: "opacusr", "mDNS advertise failed: {e}");
        return None;
    }
    tracing::info!(target: "opacusr", "mDNS: advertising {fqdn} on port {port}");
    Some(AdvertiseGuard {
        daemon,
        fqdn,
        instance,
        port,
        lan_port,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fqdn_matches_mdns_shape() {
        // Sanitized instances never carry a dot, so the fullname is
        // exactly `<instance>.<service type>` — the key unregister needs.
        assert_eq!(
            service_fqdn(&opacusr_proto::discovery::sanitize_instance_name(
                "host.local"
            )),
            "hostlocal._opacusr._tcp.local."
        );
        // The advertised TXT always carries the current protocol version.
        let props = opacusr_proto::discovery::txt_properties("/signal", None, None);
        let txt = opacusr_proto::discovery::parse_txt(&props).unwrap();
        assert_eq!(txt.proto, opacusr_proto::PROTO_VERSION);
        assert_eq!(txt.lan_port, None);
        assert_eq!(txt.peer_id, None);
    }
}
