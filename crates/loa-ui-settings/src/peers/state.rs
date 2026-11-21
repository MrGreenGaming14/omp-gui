//! The panel's interaction state and reducer; requests reach the app only while the snapshot
//! still backs them.

use super::{Data, Event, GrantDirection, GrantForm, GrantScope, Peer, PeerSort, Permission, visible};
use loa_ui_kit::widgets::{Msg, Screen};

/// A step waiting for the user's confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirm {
    RemovePeer(String),
    RevokeGrant(String),
    /// SSH access in the grant being written.
    Ssh,
}

#[derive(Debug, Clone)]
pub enum Local {
    Filter(String),
    Sort(PeerSort),
    Key(String),
    Address(String),
    AddPeer,
    Ask(Confirm),
    Confirm,
    /// Drops the pending confirmation and rename.
    Cancel,
    BeginRename(String),
    RenameText(String),
    SaveRename,
    SelectPeer(String),
    ResourceFilter(String),
    SelectResource(String),
    Permission(Permission),
    Ttl(String),
    Reshare(bool),
    FutureSessions(bool),
    FolderPath(String),
    /// Picks the typed folder as the grant's resource.
    UseFolder,
    SubmitGrant,
}

pub type Message = Msg<Local, Event>;

#[derive(Debug, Default)]
pub struct State {
    pub data: Data,
    pub filter: String,
    pub sort: PeerSort,
    pub add_key: String,
    pub add_address: String,
    /// The app's verdict on a typed key, tied to the exact text it checked.
    pub key_validation: Option<(String, bool)>,
    pub selected_peer: Option<String>,
    pub resource_filter: String,
    pub grant_form: GrantForm,
    pub confirm: Option<Confirm>,
    /// The peer being renamed and the name typed so far.
    pub rename: Option<(String, String)>,
}

impl State {
    /// Takes a new snapshot, keeping the input that it still backs.
    pub fn set_data(&mut self, data: Data) {
        let backed = match &self.confirm {
            Some(Confirm::RevokeGrant(id)) => data.grants.iter().any(|g| &g.id == id && g.direction == GrantDirection::Given),
            Some(Confirm::RemovePeer(key)) => data.peers.iter().any(|p| &p.key == key),
            _ => true,
        };
        if !backed {
            self.confirm = None;
        }
        if self.selected_peer.as_ref().is_some_and(|key| !data.peers.iter().any(|p| &p.key == key)) {
            self.selected_peer = None;
            self.grant_form = GrantForm::default();
        }
        // A typed folder isn't in the app's list; the app checks it on issue.
        if self.grant_form.resource.as_ref().is_some_and(|r| r.scope != GrantScope::Folder && !data.resources.contains(r)) {
            self.grant_form = GrantForm::default();
        }
        self.data = data;
    }

    /// Takes the app's verdict on `key` while it is still the typed one.
    pub fn set_key_validation(&mut self, key: &str, valid: bool) {
        if self.add_key.trim() == key {
            self.key_validation = Some((key.to_owned(), valid));
        }
    }

    pub fn visible_peers(&self) -> Vec<&Peer> { visible(&self.data.peers, &self.filter, self.sort) }

    /// Nearby nodes not added yet, filtered and sorted like the peers.
    pub fn visible_nearby(&self) -> Vec<&Peer> { visible(&self.data.nearby, &self.filter, self.sort).into_iter().filter(|n| !self.is_peer(&n.key)).collect() }

    pub(super) fn key_valid(&self) -> bool { self.key_validation.as_ref().is_some_and(|(key, valid)| *valid && key == self.add_key.trim()) }

    fn is_peer(&self, key: &str) -> bool { self.data.peers.iter().any(|p| p.key == key) }

    fn given(&self, id: &str) -> bool { self.data.grants.iter().any(|g| g.id == id && g.direction == GrantDirection::Given) }

    fn select_resource(&mut self, resource: Option<super::ResourceChoice>) {
        if self.confirm == Some(Confirm::Ssh) {
            self.confirm = None;
        }
        self.grant_form.select_resource(resource);
    }
}

impl Screen for State {
    type Local = Local;
    type Event = Event;

    fn reduce(&mut self, m: Local) -> Option<Event> {
        let ssh = self.grant_form.is_ssh();
        match m {
            Local::Filter(value) => self.filter = value,
            Local::Sort(sort) => self.sort = sort,
            Local::Key(value) => {
                (self.add_key, self.key_validation) = (value, None);
                return Some(Event::ValidateKey(self.add_key.trim().to_owned()));
            }
            Local::Address(value) => self.add_address = value,
            Local::AddPeer if self.key_valid() => {
                let address = self.add_address.trim();
                let event = Event::AddPeer { key: self.add_key.trim().to_owned(), address: (!address.is_empty()).then(|| address.to_owned()) };
                (self.add_key, self.add_address, self.key_validation) = Default::default();
                return Some(event);
            }
            Local::AddPeer => {}
            Local::Ask(confirm) => {
                let valid = match &confirm {
                    Confirm::RemovePeer(key) => self.is_peer(key),
                    Confirm::RevokeGrant(id) => self.given(id),
                    Confirm::Ssh => ssh,
                };
                self.confirm = valid.then_some(confirm);
            }
            Local::Confirm => match self.confirm.take()? {
                Confirm::RemovePeer(key) => return Some(Event::RemovePeer(key)),
                Confirm::RevokeGrant(id) => return Some(Event::RevokeGrant(id)),
                Confirm::Ssh => self.grant_form.ssh_confirmed = ssh,
            },
            Local::Cancel => (self.confirm, self.rename) = (None, None),
            Local::BeginRename(key) => self.rename = self.data.peers.iter().find(|p| p.key == key).map(|p| (key, p.name.clone())),
            Local::RenameText(name) => {
                if let Some((_, current)) = &mut self.rename {
                    *current = name;
                }
            }
            Local::SaveRename => {
                let (key, name) = self.rename.take()?;
                let name = name.trim();
                return (!name.is_empty()).then(|| Event::RenamePeer { key, name: name.to_owned() });
            }
            Local::SelectPeer(key) => {
                self.selected_peer = self.is_peer(&key).then_some(key);
                (self.grant_form, self.resource_filter) = Default::default();
                if self.confirm == Some(Confirm::Ssh) {
                    self.confirm = None;
                }
            }
            Local::ResourceFilter(value) => self.resource_filter = value,
            Local::SelectResource(id) => {
                let resource = self.data.resources.iter().find(|r| r.id == id).cloned();
                self.select_resource(resource);
            }
            Local::Permission(permission) => self.grant_form.permission = permission,
            Local::Ttl(value) => self.grant_form.ttl_hours = value,
            Local::Reshare(on) => self.grant_form.reshare = on,
            Local::FutureSessions(on) => self.grant_form.future_sessions = on,
            Local::FolderPath(path) => self.grant_form.folder_path = path,
            Local::UseFolder => {
                if let Some(folder) = self.grant_form.folder_choice() {
                    self.select_resource(Some(folder));
                }
            }
            Local::SubmitGrant => {
                let key = self.selected_peer.as_ref().filter(|key| self.is_peer(key))?;
                return self.grant_form.build(key).ok().map(Event::CreateGrant);
            }
        }
        None
    }

    /// Direct requests hold while the snapshot lists what they name.
    fn guard(&self, e: &Event) -> bool {
        let d = &self.data;
        match e {
            Event::AddDiscovered(key) => d.nearby.iter().any(|p| &p.key == key) && !self.is_peer(key),
            Event::CopyKey(key) => *key == d.node.key || d.peers.iter().chain(&d.nearby).any(|p| &p.key == key),
            Event::RenewGrant(id) => self.given(id),
            Event::Refresh => true,
            Event::ScanTailscale => d.node.tailscale_available,
            Event::OpenRemote { peer, session } => d.remote_sessions.iter().any(|r| &r.peer_key == peer && &r.session == session),
            Event::ForkRemote { peer, session } => d.remote_sessions.iter().any(|r| &r.peer_key == peer && &r.session == session && r.can_fork),
            Event::BrowseFolder { peer, folder } => d.shared_folders.iter().any(|f| &f.peer_key == peer && &f.path == folder && f.online),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use loa_ui_kit::widgets::{Msg, Screen};
    use super::super::*;
    use loa_ui_kit::testkit::click_label;

    fn peer(key: &str, name: &str, discovery: Discovery, presence: Presence, last_seen: Option<u64>, version: &str) -> Peer {
        Peer { key: key.into(), name: name.into(), discovery, presence, last_seen, transport: Transport::None, app_version: version.into(), shared_protocol: Some(true) }
    }

    fn state() -> State {
        let grant = Grant {
            id: "grant".into(), peer_key: "peer".into(), direction: GrantDirection::Given, permission: Permission::View, expires_at: Some(7200), renews_at: Some(3600), reshare: false, ended: false,
            resource: GrantResource { id: "resource".into(), label: "Prompt A".into(), scope: GrantScope::Prompt, future_sessions: true },
        };
        let resources = vec![ResourceChoice { id: "resource".into(), label: "Prompt A".into(), scope: GrantScope::Prompt }, ResourceChoice { id: "ssh".into(), label: "Host".into(), scope: GrantScope::SshHost }];
        let remote = RemoteItem { peer_key: "peer".into(), peer_name: "Alice".into(), session: "s1".into(), title: "Plan".into(), harness: "omp".into(), access: "view".into(), can_drive: false, can_fork: false, online: true };
        let folder = SharedFolderItem { peer_key: "peer".into(), peer_name: "Alice".into(), path: "/srv/share".into(), can_write: false, online: true };
        let nearby = vec![peer("near-z", "Zara", Discovery::Lan, Presence::Offline, Some(1), "1.0.0"), peer("near-a", "Alice nearby", Discovery::Tailscale, Presence::Online, Some(2), "1.0.0"), peer("peer", "Alice", Discovery::Lan, Presence::Online, None, "1.0.0")];
        let node = NodeCard { key: "own".into(), ..NodeCard::default() };
        let mut s = State::default();
        s.set_data(Data { node, peers: vec![peer("peer", "Alice", Discovery::Lan, Presence::Online, None, "1.0.0")], nearby, grants: vec![grant], resources, remote_sessions: vec![remote], shared_folders: vec![folder], ..Data::default() });
        s
    }

    fn input(resource: &str, label: &str, scope: GrantScope, permission: Permission, future_sessions: bool) -> Event {
        Event::CreateGrant(GrantInput { peer_key: "peer".into(), resource: GrantResource { id: resource.into(), label: label.into(), scope, future_sessions }, permission, ttl_seconds: 24 * 3600, reshare: false })
    }

    #[test]
    fn messages_reduce_to_guarded_requests() {
        use {Local as L, Permission as P};
        let l = |m: L| Msg::Local(m);
        let e = |e: Event| Msg::Emit(e);
        let select = |id: &str| [l(L::SelectPeer("peer".into())), l(L::SelectResource(id.into()))];
        let remote = |session: &str| Event::OpenRemote { peer: "peer".into(), session: session.into() };
        let fork = Event::ForkRemote { peer: "peer".into(), session: "s1".into() };
        let browse = |folder: &str| Event::BrowseFolder { peer: "peer".into(), folder: folder.into() };
        let ask = |c: Confirm| l(L::Ask(c));
        // (messages, requests that reach the app)
        let cases: Vec<(Vec<Message>, Vec<Event>)> = vec![
            (vec![l(L::Key("key-one".into())), l(L::Key("key-two".into())), l(L::AddPeer)], vec![Event::ValidateKey("key-one".into()), Event::ValidateKey("key-two".into())]),
            ([&select("ssh")[..], &[l(L::SubmitGrant), l(L::Confirm), ask(Confirm::Ssh), l(L::Confirm), l(L::SubmitGrant), l(L::Ttl("0".into())), l(L::SubmitGrant)]].concat(), vec![input("ssh", "Host", GrantScope::SshHost, P::View, false)]),
            ([&select("ssh")[..], &[ask(Confirm::Ssh), l(L::SelectResource("resource".into())), l(L::SelectResource("ssh".into())), l(L::Confirm), l(L::SubmitGrant)]].concat(), vec![]),
            ([&select("resource")[..], &[l(L::FutureSessions(true)), l(L::Permission(P::View)), l(L::SubmitGrant)]].concat(), vec![input("resource", "Prompt A", GrantScope::Prompt, P::View, true)]),
            ([&select("resource")[..], &[l(L::SubmitGrant)]].concat(), vec![input("resource", "Prompt A", GrantScope::Prompt, P::Fork, false)]),
            (vec![l(L::SelectPeer("peer".into())), l(L::UseFolder), l(L::FolderPath(" /srv/share ".into())), l(L::UseFolder), l(L::Permission(P::Fork)), l(L::SubmitGrant), l(L::Permission(P::Drive)), l(L::SubmitGrant)], vec![input("folder:/srv/share", "Folder /srv/share", GrantScope::Folder, P::Drive, false)]),
            (vec![ask(Confirm::RemovePeer("peer".into())), l(L::Cancel), l(L::Confirm), ask(Confirm::RemovePeer("peer".into())), l(L::Confirm)], vec![Event::RemovePeer("peer".into())]),
            (vec![l(L::BeginRename("peer".into())), l(L::RenameText("  ".into())), l(L::SaveRename), l(L::BeginRename("peer".into())), l(L::RenameText(" New name ".into())), l(L::SaveRename)], vec![Event::RenamePeer { key: "peer".into(), name: "New name".into() }]),
            (vec![ask(Confirm::RevokeGrant("other".into())), l(L::Confirm), ask(Confirm::RevokeGrant("grant".into())), l(L::Confirm)], vec![Event::RevokeGrant("grant".into())]),
            (vec![e(Event::RenewGrant("grant".into())), e(Event::RenewGrant("other".into())), e(Event::CopyKey("own".into())), e(Event::CopyKey("stranger".into())), e(Event::ScanTailscale), e(Event::Refresh)], vec![Event::RenewGrant("grant".into()), Event::CopyKey("own".into()), Event::Refresh]),
            (vec![e(remote("s1")), e(remote("s2")), e(fork), e(browse("/srv/share")), e(browse("/etc")), e(Event::AddDiscovered("peer".into())), e(Event::AddDiscovered("near-a".into()))], vec![remote("s1"), browse("/srv/share"), Event::AddDiscovered("near-a".into())]),
        ];
        for (i, (messages, want)) in cases.into_iter().enumerate() {
            let mut s = state();
            let events: Vec<Event> = messages.into_iter().filter_map(|m| s.update(m)).collect();
            assert_eq!(events, want, "case {i}");
        }
    }

    #[test]
    fn snapshots_keep_only_backed_input() {
        let mut s = state();
        s.set_key_validation("key", true);
        s.update(Msg::Local(Local::Key("key".into())));
        s.set_key_validation("key", true);
        s.update(Msg::Local(Local::Address(" 127.0.0.1:31415 ".into())));
        assert_eq!(s.update(Msg::Local(Local::AddPeer)), Some(Event::AddPeer { key: "key".into(), address: Some("127.0.0.1:31415".into()) }));
        for m in [Local::SelectPeer("peer".into()), Local::SelectResource("resource".into()), Local::Ask(Confirm::RevokeGrant("grant".into()))] {
            s.update(Msg::Local(m));
        }
        let mut snapshot = s.data.clone();
        (snapshot.grants, snapshot.resources) = Default::default();
        s.set_data(snapshot);
        assert_eq!((s.update(Msg::Local(Local::Confirm)), s.update(Msg::Local(Local::SubmitGrant))), (None, None));
        s.data.remote_sessions[0].can_fork = true;
        s.data.node.tailscale_available = true;
        s.data.shared_folders[0].online = false;
        let events: Vec<_> = [Event::ForkRemote { peer: "peer".into(), session: "s1".into() }, Event::ScanTailscale, Event::BrowseFolder { peer: "peer".into(), folder: "/srv/share".into() }].into_iter().filter_map(|e| s.update(Msg::Emit(e))).collect();
        assert_eq!(events, [Event::ForkRemote { peer: "peer".into(), session: "s1".into() }, Event::ScanTailscale]);
    }

    #[test]
    fn lists_filter_sort_and_describe() {
        let mut s = state();
        let keys = |list: Vec<&Peer>| list.into_iter().map(|p| p.key.clone()).collect::<Vec<_>>();
        s.sort = PeerSort::OnlineFirst;
        assert_eq!(keys(s.visible_nearby()), ["near-a", "near-z"]);
        s.filter = "zara".into();
        assert_eq!(keys(s.visible_nearby()), ["near-z"]);
        let peers = vec![peer("z", "Zed", Discovery::Lan, Presence::Offline, Some(20), "1.0.0"), peer("a", "Alice", Discovery::Lan, Presence::Online, Some(10), "2.0.0"), peer("b", "Bob", Discovery::Lan, Presence::Offline, Some(30), "1.5.0")];
        for (filter, sort, want) in [("ALi", PeerSort::Name, &["a"][..]), ("", PeerSort::OnlineFirst, &["a", "b", "z"]), ("", PeerSort::RecentlySeen, &["b", "z", "a"]), ("", PeerSort::Version, &["a", "b", "z"])] {
            assert_eq!(keys(visible(&peers, filter, sort)), want, "{sort}");
        }
        let mut node = peers[1].clone();
        node.app_version = "v1.2.3".into();
        assert_eq!((node.version_relation("1.4.0"), node.version_relation("1.2.3+build"), node.version_relation("x")), (VersionRelation::Behind, VersionRelation::Current, VersionRelation::Unknown));
        assert_eq!((peers[0].last_seen_text(150), peers[1].last_seen_text(0)), ("2m ago".to_owned(), "now".to_owned()));
        let grant = &state().data.grants[0];
        assert_eq!((grant_summary(grant), grant.expiry_text(0), grant.expiry_text(7300)), ("Prompt A and every session from now on, view".into(), "expires in 2h; renews in 1h".into(), "expired; renewal due".into()));
        let mut form = GrantForm::default();
        for (scope, permission) in [(GrantScope::Surface, Permission::View), (GrantScope::SshHost, Permission::View), (GrantScope::Sandbox, Permission::Drive), (GrantScope::Session, Permission::Fork)] {
            form.select_resource(Some(ResourceChoice { id: format!("{scope:?}"), label: String::new(), scope }));
            assert_eq!(form.permission, permission, "{scope:?}");
        }
    }

    #[test]
    fn the_panel_selects_peers_and_copies_keys() {
        let mut s = state();
        let mut events: Vec<Event> = click_label(s.view(), "Grants").into_iter().filter_map(|m| s.update(m)).collect();
        assert_eq!(s.selected_peer.as_deref(), Some("peer"));
        events.extend(click_label(s.view(), "Copy key").into_iter().filter_map(|m| s.update(m)));
        assert_eq!(events, [Event::CopyKey("own".into())]);
    }
}
