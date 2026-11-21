//! The mDNS adapter: announces this node and browses for others on the LAN.

use super::{Availability, Discoverer, NearbyPeer, NearbySource};
use crate::{NodeId, Result, net};
use async_trait::async_trait;
use mdns_sd::{Receiver, ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::SocketAddr;

pub const SERVICE_TYPE: &str = "_loa._udp.local.";
const KEY: &str = "node";
const TCP: &str = "tcp";
const NAME: &str = "name";
/// DNS labels hold 63 bytes: labels carry a key prefix, TXT the whole key.
const LABEL_KEY_CHARS: usize = 16;

/// What a node announces about itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Announce {
    pub node: NodeId,
    pub udp_port: u16,
    pub tcp_port: u16,
    pub name: String,
}

impl Announce {
    /// The service record, without starting multicast.
    pub fn info(&self) -> Result<ServiceInfo> {
        if self.udp_port == 0 || self.tcp_port == 0 {
            return Err(crate::NodeError::Net("mDNS ports must not be zero".into()));
        }
        let key = self.node.to_string();
        let label = format!("loa-{}", &key[..LABEL_KEY_CHARS]);
        let tcp = self.tcp_port.to_string();
        let mut properties = vec![(KEY, key.as_str()), (TCP, tcp.as_str())];
        if !self.name.is_empty() {
            properties.push((NAME, self.name.as_str()));
        }
        ServiceInfo::new(SERVICE_TYPE, &label, &format!("{label}.local."), "", self.udp_port, properties.as_slice())
            .map(ServiceInfo::enable_addr_auto)
            .map_err(net)
    }
}

/// A resolved announcement as a nearby node; `None` when incomplete.
pub fn parse(service: &ResolvedService) -> Option<NearbyPeer> {
    let port = service.get_port();
    if service.ty_domain != SERVICE_TYPE || port == 0 {
        return None;
    }
    let node = service.get_property_val_str(KEY)?.parse().ok()?;
    let tcp = match service.get_property_val_str(TCP) {
        Some(text) => text.parse().ok().filter(|p| *p != 0)?,
        None => port,
    };
    let mut ips: Vec<_> = service.get_addresses().iter().map(|ip| ip.to_ip_addr()).collect();
    ips.sort();
    let mut addresses: Vec<SocketAddr> = ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect();
    if tcp != port {
        addresses.extend(ips.iter().map(|ip| SocketAddr::new(*ip, tcp)));
    }
    let name = service.get_property_val_str(NAME).filter(|n| !n.is_empty()).map(str::to_owned);
    Some(NearbyPeer::new(node, NearbySource::Mdns, name, addresses))
}

/// A registration and browse subscription; dropping it unregisters.
pub struct Mdns {
    daemon: ServiceDaemon,
    events: Receiver<ServiceEvent>,
    fullname: String,
    own: NodeId,
}

impl Mdns {
    pub fn start(announce: &Announce) -> Result<Self> {
        let info = announce.info()?;
        let daemon = ServiceDaemon::new().map_err(net)?;
        let started = daemon.browse(SERVICE_TYPE).and_then(|events| daemon.register(info.clone()).map(|()| events));
        match started {
            Ok(events) => Ok(Self { daemon, events, fullname: info.get_fullname().to_owned(), own: announce.node }),
            Err(error) => {
                let _ = daemon.shutdown();
                Err(net(error))
            }
        }
    }
}

#[async_trait]
impl Discoverer for Mdns {
    async fn next(&mut self) -> Option<NearbyPeer> {
        while let Ok(event) = self.events.recv_async().await {
            if let ServiceEvent::ServiceResolved(service) = event
                && let Some(peer) = parse(&service).filter(|p| p.key != self.own)
            {
                return Some(peer);
            }
        }
        None
    }

    fn availability(&self) -> Availability { Availability::Active("announcing and browsing on the LAN".into()) }
}

impl Drop for Mdns {
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
    fn announcement_round_trips_without_multicast() {
        let node = iroh::SecretKey::from_bytes(&[7; 32]).public();
        let info = Announce { node, udp_port: 31415, tcp_port: 31416, name: "seven".into() }.info().unwrap();
        assert!(info.get_fullname().split('.').all(|label| label.len() <= 63));
        let mut resolved = info.as_resolved_service();
        let peer = parse(&resolved).unwrap();
        assert_eq!((peer.key, peer.name.as_deref(), peer.source), (node, Some("seven"), NearbySource::Mdns));
        resolved.port = 0;
        assert_eq!(parse(&resolved), None);
        assert!(Announce { node, udp_port: 0, tcp_port: 1, name: String::new() }.info().is_err());
    }
}
