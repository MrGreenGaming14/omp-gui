//! Peers: this node, its peers and the nodes seen nearby, grants given and received (with the
//! form issuing one), and the sessions and folders peers share with this node. The app supplies
//! a [`Data`](loa_view::settings::peers::Data) snapshot and carries out the
//! [`Intent`](loa_view::settings::peers::Intent)s; tokens, keys and the network stay there.

mod grant_form;
mod state;
mod view;

pub use grant_form::{GrantForm, GrantFormError, grant_summary};
pub use state::{Confirm, Local, Message, State};

use loa_view::settings::peers::{Peer, Presence};
use std::cmp::Ordering;

loa_util::str_enum! {
    #[derive(Default)]
    pub PeerSort { #[default] Name => "Name", OnlineFirst => "Online first", RecentlySeen => "Recently seen", Version => "App version" }
}

/// `peers` matching `filter` by name, key or discovery, sorted by `sort` then name and key.
pub(crate) fn visible<'a>(peers: &'a [Peer], filter: &str, sort: PeerSort) -> Vec<&'a Peer> {
    let term = filter.trim().to_lowercase();
    let mut visible: Vec<_> = peers.iter().filter(|p| [p.name.as_str(), &p.key, p.discovery.as_str()].iter().any(|s| s.to_lowercase().contains(&term))).collect();
    visible.sort_by(|a, b| {
        let order = match sort {
            PeerSort::Name => Ordering::Equal,
            PeerSort::OnlineFirst => (a.presence != Presence::Online).cmp(&(b.presence != Presence::Online)),
            PeerSort::RecentlySeen => b.last_seen.cmp(&a.last_seen),
            PeerSort::Version => b.version().cmp(&a.version()),
        };
        order.then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then_with(|| a.key.cmp(&b.key))
    });
    visible
}
