//! LAN service discovery. Advertisements are hints, not authenticated identities:
//! dialers must verify the advertised key during the transport handshake.

use crate::NodeId;
use crate::error::{NetError, Result};
use mdns_sd::{Receiver, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::SocketAddr;

/// mDNS service for the UDP iroh endpoint.
pub const SERVICE_TYPE: &str = "_loa._udp.local.";
const KEY_PROPERTY: &str = "node";
const TCP_PROPERTY: &str = "tcp";
const NAME_PROPERTY: &str = "name";
/// DNS labels hold at most 63 bytes; a node key is 64 hex characters, so the
/// instance and host labels use a prefix and the full key travels in TXT.
const LABEL_KEY_CHARS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredPeer {
    pub node: NodeId,
    /// UDP port of the iroh endpoint.
    pub port: u16,
    /// TCP fallback port; the UDP port when the announcement names none.
    pub tcp_port: u16,
    pub hostname: String,
    /// The node's display name, when announced.
    pub name: Option<String>,
    /// Resolved addresses with the UDP port, then with the TCP port when it differs.
    pub addresses: Vec<SocketAddr>,
}

/// What a node announces about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    pub node: NodeId,
    pub udp_port: u16,
    pub tcp_port: u16,
    pub name: String,
}

/// Builds the announcement without starting multicast, useful also for offline tests.
pub fn announcement(announce: &Announce) -> Result<ServiceInfo> {
    if announce.udp_port == 0 || announce.tcp_port == 0 {
        return Err(NetError::InvalidAddress("mDNS ports must not be zero".into()));
    }
    let key = announce.node.to_string();
    let label = format!("loa-{}", &key[..LABEL_KEY_CHARS]);
    let tcp = announce.tcp_port.to_string();
    let mut properties = vec![(KEY_PROPERTY, key.as_str()), (TCP_PROPERTY, tcp.as_str())];
    if !announce.name.is_empty() {
        properties.push((NAME_PROPERTY, announce.name.as_str()));
    }
    ServiceInfo::new(SERVICE_TYPE, &label, &format!("{label}.local."), "", announce.udp_port, properties.as_slice())
        .map(ServiceInfo::enable_addr_auto)
        .map_err(|err| NetError::Mdns(err.to_string()))
}

/// Parses a resolved advertisement; unparseable or incomplete announcements are ignored.
pub fn parse_resolved(service: &ResolvedService) -> Option<DiscoveredPeer> {
    if service.ty_domain != SERVICE_TYPE || service.get_port() == 0 {
        return None;
    }
    let node = service.get_property_val_str(KEY_PROPERTY)?.parse().ok()?;
    let port = service.get_port();
    let tcp_port = match service.get_property_val_str(TCP_PROPERTY) {
        Some(text) => text.parse().ok().filter(|p| *p != 0)?,
        None => port,
    };
    let mut ips: Vec<_> = service.get_addresses().iter().map(|ip| ip.to_ip_addr()).collect();
    ips.sort();
    let mut addresses: Vec<SocketAddr> = ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect();
    if tcp_port != port {
        addresses.extend(ips.iter().map(|ip| SocketAddr::new(*ip, tcp_port)));
    }
    Some(DiscoveredPeer {
        node,
        port,
        tcp_port,
        hostname: service.get_hostname().to_owned(),
        name: service.get_property_val_str(NAME_PROPERTY).filter(|n| !n.is_empty()).map(str::to_owned),
        addresses,
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
    own: NodeId,
}

impl Discovery {
    /// Announces `announce` on the LAN and browses for other nodes. Disabled
    /// discovery opens no sockets.
    pub fn new(announce: &Announce, enabled: bool) -> Result<Self> {
        if !enabled {
            return Ok(Self { active: None });
        }
        let info = announcement(announce)?;
        let daemon = ServiceDaemon::new().map_err(|err| NetError::Mdns(err.to_string()))?;
        let result: Result<Active> = (|| {
            let events = daemon.browse(SERVICE_TYPE).map_err(|err| NetError::Mdns(err.to_string()))?;
            daemon.register(info.clone()).map_err(|err| NetError::Mdns(err.to_string()))?;
            Ok(Active { daemon: daemon.clone(), events, fullname: info.get_fullname().to_owned(), own: announce.node })
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

    /// Waits for the next resolved node other than this one; removal events are
    /// not reported as discoveries. Returns `None` when disabled or when the
    /// daemon closes its subscription.
    pub async fn next_peer(&self) -> Option<DiscoveredPeer> {
        let active = self.active.as_ref()?;
        while let Ok(event) = active.events.recv_async().await {
            if let ServiceEvent::ServiceResolved(service) = event
                && let Some(peer) = parse_resolved(&service)
                && peer.node != active.own
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

    fn announce(seed: u8, udp_port: u16, tcp_port: u16) -> Announce {
        Announce { node: iroh::SecretKey::from_bytes(&[seed; 32]).public(), udp_port, tcp_port, name: format!("node-{seed}") }
    }

    #[test]
    fn announcement_round_trip_without_multicast() {
        let announce = announce(7, 31415, 31416);
        let node = announce.node;
        let info = announcement(&announce).unwrap();
        assert_eq!(info.get_type(), SERVICE_TYPE);
        assert_eq!(info.get_port(), 31415);
        assert_eq!(info.get_property_val_str(KEY_PROPERTY), Some(node.to_string().as_str()));
        assert!(info.is_addr_auto());
        assert!(info.get_fullname().split('.').all(|label| label.len() <= 63), "{}", info.get_fullname());
        assert!(info.get_hostname().split('.').all(|label| label.len() <= 63));
        let hostname = info.get_hostname().to_owned();
        let resolved = info.as_resolved_service();
        assert_eq!(parse_resolved(&resolved), Some(DiscoveredPeer {
            node,
            port: 31415,
            tcp_port: 31416,
            hostname,
            name: Some("node-7".into()),
            addresses: Vec::new(),
        }));
        let mut malformed = resolved;
        malformed.port = 0;
        assert_eq!(parse_resolved(&malformed), None);
        malformed.port = 31415;
        malformed.ty_domain = "_other._udp.local.".into();
        assert_eq!(parse_resolved(&malformed), None);
        assert!(announcement(&self::announce(7, 0, 1)).is_err());
    }

    #[test]
    fn disabled_discovery_does_not_open_multicast() {
        let discovery = Discovery::new(&announce(7, 0, 0), false).unwrap();
        assert!(!discovery.enabled());
    }

    #[tokio::test]
    #[ignore = "uses LAN multicast on the host's interfaces"]
    async fn two_daemons_find_each_other_over_multicast() {
        let a = announce(11, 40001, 40002);
        let b = announce(12, 40003, 40004);
        let found_by_a = Discovery::new(&a, true).unwrap();
        let found_by_b = Discovery::new(&b, true).unwrap();
        let seen = tokio::time::timeout(std::time::Duration::from_secs(10), found_by_a.next_peer()).await.unwrap().unwrap();
        assert_eq!(seen.node, b.node);
        assert_eq!((seen.port, seen.tcp_port), (40003, 40004));
        assert_eq!(seen.name.as_deref(), Some("node-12"));
        assert!(!seen.addresses.is_empty());
        let seen = tokio::time::timeout(std::time::Duration::from_secs(10), found_by_b.next_peer()).await.unwrap().unwrap();
        assert_eq!(seen.node, a.node);
    }
}
