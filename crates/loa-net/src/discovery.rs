//! LAN service discovery. Advertisements are hints, not authenticated identities:
//! dialers must verify the advertised key during the transport handshake.

use crate::NodeId;
use crate::error::{NetError, Result};
use mdns_sd::{Receiver, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};

/// mDNS service for the UDP iroh endpoint.
pub const SERVICE_TYPE: &str = "_loa._udp.local.";
const KEY_PROPERTY: &str = "node";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub node: NodeId,
    pub port: u16,
    pub hostname: String,
}

/// Builds the announcement without starting multicast, useful also for offline tests.
pub fn announcement(node: NodeId, port: u16) -> Result<ServiceInfo> {
    if port == 0 {
        return Err(NetError::InvalidAddress("mDNS port must not be zero".into()));
    }
    let key = node.to_string();
    ServiceInfo::new(
        SERVICE_TYPE,
        &key,
        &format!("loa-{key}.local."),
        "",
        port,
        [(KEY_PROPERTY, key.as_str())].as_slice(),
    )
    .map(ServiceInfo::enable_addr_auto)
    .map_err(|err| NetError::Mdns(err.to_string()))
}

/// Parses a resolved advertisement; unparseable or incomplete announcements are ignored.
pub fn parse_resolved(service: &ResolvedService) -> Option<DiscoveredPeer> {
    if service.ty_domain != SERVICE_TYPE || service.get_port() == 0 {
        return None;
    }
    let node = service.get_property_val_str(KEY_PROPERTY)?.parse().ok()?;
    Some(DiscoveredPeer {
        node,
        port: service.get_port(),
        hostname: service.get_hostname().to_owned(),
    })
}

/// An optional mDNS registration and browse subscription. Dropping it unregisters
/// our announcement and stops browsing; disabled mode opens no sockets.
pub struct Discovery {
    active: Option<Active>,
}

struct Active {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    fullname: String,
}

impl Discovery {
    pub fn new(node: NodeId, port: u16, enabled: bool) -> Result<Self> {
        if !enabled {
            return Ok(Self { active: None });
        }
        let info = announcement(node, port)?;
        let daemon = ServiceDaemon::new().map_err(|err| NetError::Mdns(err.to_string()))?;
        let result: Result<Active> = (|| {
            let events = daemon.browse(SERVICE_TYPE).map_err(|err| NetError::Mdns(err.to_string()))?;
            daemon.register(info.clone()).map_err(|err| NetError::Mdns(err.to_string()))?;
            Ok(Active { daemon: daemon.clone(), events, fullname: info.get_fullname().to_owned() })
        })();
        if result.is_err() {
            let _ = daemon.stop_browse(SERVICE_TYPE);
            let _ = daemon.shutdown();
        }
        Ok(Self { active: Some(result?) })
    }

    pub fn enabled(&self) -> bool {
        self.active.is_some()
    }

    /// Waits for the next resolved node; removal events are not reported as discoveries.
    /// Returns `None` when disabled or when the daemon closes its subscription.
    pub async fn next_peer(&self) -> Option<DiscoveredPeer> {
        let active = self.active.as_ref()?;
        while let Ok(event) = active.events.recv_async().await {
            if let ServiceEvent::ServiceResolved(service) = event
                && let Some(peer) = parse_resolved(&service)
            {
                return Some(peer);
            }
        }
        None
    }
}

impl Drop for Active {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.stop_browse(SERVICE_TYPE);
        let _ = self.daemon.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn announcement_round_trip_without_multicast() {
        let node = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let info = announcement(node, 31415).unwrap();
        assert_eq!(info.get_type(), SERVICE_TYPE);
        assert_eq!(info.get_port(), 31415);
        assert_eq!(info.get_property_val_str(KEY_PROPERTY), Some(node.to_string().as_str()));
        assert!(info.is_addr_auto());
        let hostname = info.get_hostname().to_owned();
        let resolved = info.as_resolved_service();
        assert_eq!(parse_resolved(&resolved), Some(DiscoveredPeer {
            node,
            port: 31415,
            hostname,
        }));
        let mut malformed = resolved;
        malformed.port = 0;
        assert_eq!(parse_resolved(&malformed), None);
        malformed.port = 31415;
        malformed.ty_domain = "_other._udp.local.".into();
        assert_eq!(parse_resolved(&malformed), None);
    }

    #[test]
    fn disabled_discovery_does_not_open_multicast() {
        let node = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let discovery = Discovery::new(node, 0, false).unwrap();
        assert!(!discovery.enabled());
    }
}
