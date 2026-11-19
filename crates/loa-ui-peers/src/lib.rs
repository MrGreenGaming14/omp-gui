//! Peer discovery and capability-grant screens driven by app-owned data.

mod grants;
mod peers;
mod style;

pub use grants::{
    Grant, GrantDirection, GrantForm, GrantFormError, GrantInput, GrantResource, GrantScope,
    Permission, ResourceChoice, grant_summary,
};
pub use peers::{Discovery, Peer, PeerSort, Presence, Transport, VersionRelation};

use iced::widget::{button, column, container, row, scrollable, space, text, text_input};
use iced::{Element, Fill, Length, Task};
use loa_ui_icons::{Icon, muted};

const SMALL: f32 = 12.0;

/// This node as the panel shows it.
#[derive(Debug, Clone, Default)]
pub struct NodeCard {
    pub key: String,
    pub name: String,
    /// Where the node key lives, e.g. "OS keychain".
    pub key_source: String,
    /// The endpoint's state, e.g. "UDP 0.0.0.0:31415 · TCP 0.0.0.0:31415".
    pub endpoint: String,
    pub endpoint_ok: bool,
    /// LAN (mDNS) discovery state.
    pub lan: String,
    pub lan_ok: bool,
    /// Tailscale discovery state.
    pub tailscale: String,
    pub tailscale_ok: bool,
    /// The tailnet can be scanned now.
    pub tailscale_available: bool,
}

/// A peer's session this node's grants reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteItem {
    pub peer_key: String,
    pub peer_name: String,
    pub session: String,
    /// Title, or the session id when the grant hides it.
    pub title: String,
    pub harness: String,
    /// What the grants allow, e.g. "view, drive".
    pub access: String,
    pub can_drive: bool,
    pub can_fork: bool,
    pub online: bool,
}

/// A peer's folder this node's grants reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SharedFolderItem {
    pub peer_key: String,
    pub peer_name: String,
    /// The folder's path on the peer.
    pub path: String,
    pub can_write: bool,
    pub online: bool,
}

/// App-supplied snapshot consumed by the two screens.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub node: NodeCard,
    pub peers: Vec<Peer>,
    pub nearby: Vec<Peer>,
    pub grants: Vec<Grant>,
    pub resources: Vec<ResourceChoice>,
    pub remote_sessions: Vec<RemoteItem>,
    pub shared_folders: Vec<SharedFolderItem>,
    pub local_version: String,
    /// Current UTC instant in seconds; the app refreshes it to update countdowns.
    pub now: u64,
}

/// A user request requiring the app's peer, clipboard or grant services.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    ValidateKey(String),
    AddPeer { key: String, address: Option<String> },
    AddDiscovered(String),
    CopyKey(String),
    RemovePeer(String),
    RenamePeer { key: String, name: String },
    CreateGrant(GrantInput),
    RevokeGrant(String),
    /// Extend a given grant by its original duration and resend it.
    RenewGrant(String),
    /// Probe every peer and refresh what they share.
    Refresh,
    ScanTailscale,
    /// Open a peer's session in the center view.
    OpenRemote { peer: String, session: String },
    /// Copy a peer's session into a new session on this node.
    ForkRemote { peer: String, session: String },
    /// Browse a peer's shared folder in the center view.
    BrowseFolder { peer: String, folder: String },
}

/// Input from the widgets; the app handles the resulting `Event`.
#[derive(Debug, Clone)]
pub enum Message {
    FilterChanged(String),
    SortChanged(PeerSort),
    KeyChanged(String),
    AddressChanged(String),
    AddPeer,
    AddDiscovered(String),
    CopyKey(String),
    BeginRemove(String),
    CancelRemove,
    ConfirmRemove,
    BeginRename(String),
    RenameChanged(String),
    CancelRename,
    SaveRename,
    SelectPeer(String),
    ResourceFilterChanged(String),
    SelectResource(String),
    SelectPermission(Permission),
    TtlChanged(String),
    ReshareChanged(bool),
    FutureSessionsChanged(bool),
    RequestSshConfirmation,
    ConfirmSsh,
    CancelSsh,
    SubmitGrant,
    BeginRevoke(String),
    CancelRevoke,
    ConfirmRevoke,
    RenewGrant(String),
    Refresh,
    ScanTailscale,
    OpenRemote { peer: String, session: String },
    ForkRemote { peer: String, session: String },
    FolderPathChanged(String),
    /// Pick the typed folder as the grant's resource.
    UseFolder,
    BrowseFolder { peer: String, folder: String },
}

/// Resource choices shown at once; the filter narrows longer lists.
const RESOURCE_ROWS: usize = 8;

/// Local interaction state, independent of the network and grant store.
#[derive(Debug, Default)]
pub struct State {
    pub data: Data,
    pub filter: String,
    pub sort: PeerSort,
    pub add_key: String,
    pub add_address: String,
    /// Result of app-side public-key validation, tied to the exact text checked.
    pub key_validation: Option<(String, bool)>,
    pub selected_peer: Option<String>,
    pub resource_filter: String,
    pub grant_form: GrantForm,
    pub pending_revoke: Option<String>,
    pub pending_remove: Option<String>,
    pub rename: Option<(String, String)>,
}

impl State {
    /// Replace an app snapshot without discarding in-progress form input.
    pub fn set_data(&mut self, data: Data) {
        if self.pending_revoke.as_ref().is_some_and(|id| !data.grants.iter().any(|g| &g.id == id && g.direction == GrantDirection::Given)) {
            self.pending_revoke = None;
        }
        if self.pending_remove.as_ref().is_some_and(|key| !data.peers.iter().any(|p| &p.key == key)) {
            self.pending_remove = None;
        }
        if self.selected_peer.as_ref().is_some_and(|key| !data.peers.iter().any(|p| &p.key == key)) {
            self.selected_peer = None;
            self.grant_form = GrantForm::default();
        }
        // A typed folder isn't in the app's list; the app checks it on issue.
        if self.grant_form.resource.as_ref().is_some_and(|resource| {
            resource.scope != GrantScope::Folder && !data.resources.iter().any(|r| r == resource)
        }) {
            self.grant_form = GrantForm::default();
        }
        self.data = data;
    }

    /// Accept an asynchronous validation result only for the current key.
    pub fn set_key_validation(&mut self, key: &str, valid: bool) {
        if self.add_key.trim() == key {
            self.key_validation = Some((key.to_owned(), valid));
        }
    }

    /// Return the filtered, sorted list, with a stable public-key tie break.
    pub fn visible_peers(&self) -> Vec<&Peer> {
        peers::visible_peers(&self.data.peers, &self.filter, self.sort)
    }

    /// Return nearby unadded nodes using the same filter and sort as saved peers.
    pub fn visible_nearby(&self) -> Vec<&Peer> {
        peers::visible_peers(&self.data.nearby, &self.filter, self.sort)
            .into_iter()
            .filter(|peer| !self.data.peers.iter().any(|added| added.key == peer.key))
            .collect()
    }

    /// Apply an interaction and return an app-owned effect if one is required.
    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let event = match message {
            Message::FilterChanged(value) => { self.filter = value; None }
            Message::SortChanged(value) => { self.sort = value; None }
            Message::KeyChanged(value) => {
                self.add_key = value;
                self.key_validation = None;
                Some(Event::ValidateKey(self.add_key.trim().to_owned()))
            }
            Message::AddressChanged(value) => { self.add_address = value; None }
            Message::AddPeer => {
                let key = self.add_key.trim();
                if self.key_validation.as_ref().is_some_and(|(validated, valid)| *valid && validated == key) {
                    let event = Event::AddPeer {
                        key: key.to_owned(),
                        address: (!self.add_address.trim().is_empty()).then(|| self.add_address.trim().to_owned()),
                    };
                    self.add_key.clear();
                    self.add_address.clear();
                    self.key_validation = None;
                    Some(event)
                } else { None }
            }
            Message::AddDiscovered(key) => (self.data.nearby.iter().any(|p| p.key == key)
                && !self.data.peers.iter().any(|p| p.key == key)).then_some(Event::AddDiscovered(key)),
            Message::CopyKey(key) => (key == self.data.node.key || self.data.peers.iter().chain(&self.data.nearby).any(|p| p.key == key))
                .then_some(Event::CopyKey(key)),
            Message::BeginRemove(key) => { self.pending_remove = self.data.peers.iter().any(|p| p.key == key).then_some(key); None }
            Message::CancelRemove => { self.pending_remove = None; None }
            Message::ConfirmRemove => self.pending_remove.take().map(Event::RemovePeer),
            Message::BeginRename(key) => {
                self.rename = self.data.peers.iter().find(|p| p.key == key).map(|p| (key, p.name.clone()));
                None
            }
            Message::RenameChanged(name) => { if let Some((_, current)) = &mut self.rename { *current = name; } None }
            Message::CancelRename => { self.rename = None; None }
            Message::SaveRename => self.rename.take().and_then(|(key, name)| {
                let name = name.trim();
                (!name.is_empty()).then(|| Event::RenamePeer { key, name: name.to_owned() })
            }),
            Message::SelectPeer(key) => {
                self.selected_peer = self.data.peers.iter().any(|p| p.key == key).then_some(key);
                self.grant_form = GrantForm::default();
                self.resource_filter.clear();
                None
            }
            Message::ResourceFilterChanged(value) => { self.resource_filter = value; None }
            Message::SelectResource(id) => {
                self.grant_form.select_resource(self.data.resources.iter().find(|r| r.id == id).cloned());
                None
            }
            Message::SelectPermission(permission) => { self.grant_form.permission = permission; None }
            Message::TtlChanged(value) => { self.grant_form.ttl_hours = value; None }
            Message::ReshareChanged(value) => { self.grant_form.reshare = value; None }
            Message::FutureSessionsChanged(value) => { self.grant_form.future_sessions = value; None }
            Message::RequestSshConfirmation => { if self.grant_form.is_ssh() { self.grant_form.ssh_confirmation_pending = true; } None }
            Message::ConfirmSsh => {
                if self.grant_form.ssh_confirmation_pending && self.grant_form.is_ssh() {
                    self.grant_form.ssh_confirmed = true;
                }
                self.grant_form.ssh_confirmation_pending = false;
                None
            }
            Message::CancelSsh => { self.grant_form.ssh_confirmation_pending = false; None }
            Message::SubmitGrant => self.selected_peer.as_ref()
                .and_then(|key| self.data.peers.iter().any(|p| &p.key == key).then_some(key))
                .and_then(|key| self.grant_form.build(key).ok())
                .map(Event::CreateGrant),
            Message::BeginRevoke(id) => {
                self.pending_revoke = self.data.grants.iter().any(|g| g.id == id && g.direction == GrantDirection::Given).then_some(id);
                None
            }
            Message::CancelRevoke => { self.pending_revoke = None; None }
            Message::ConfirmRevoke => self.pending_revoke.take().map(Event::RevokeGrant),
            Message::RenewGrant(id) => self.data.grants.iter()
                .any(|g| g.id == id && g.direction == GrantDirection::Given)
                .then_some(Event::RenewGrant(id)),
            Message::Refresh => Some(Event::Refresh),
            Message::ScanTailscale => self.data.node.tailscale_available.then_some(Event::ScanTailscale),
            Message::OpenRemote { peer, session } => self.data.remote_sessions.iter()
                .any(|r| r.peer_key == peer && r.session == session)
                .then_some(Event::OpenRemote { peer, session }),
            Message::ForkRemote { peer, session } => self.data.remote_sessions.iter()
                .any(|r| r.peer_key == peer && r.session == session && r.can_fork)
                .then_some(Event::ForkRemote { peer, session }),
            Message::FolderPathChanged(path) => { self.grant_form.folder_path = path; None }
            Message::UseFolder => {
                if let Some(folder) = self.grant_form.folder_choice() {
                    self.grant_form.select_resource(Some(folder));
                }
                None
            }
            Message::BrowseFolder { peer, folder } => self.data.shared_folders.iter()
                .any(|f| f.peer_key == peer && f.path == folder && f.online)
                .then_some(Event::BrowseFolder { peer, folder }),
        };
        (Task::none(), event)
    }

    /// Render the peers list and the selected peer's grant management controls.
    pub fn view(&self) -> Element<'_, Message> {
        let sort = iced::widget::pick_list(PeerSort::ALL, Some(self.sort), Message::SortChanged)
            .text_size(13)
            .padding([5.0, 8.0])
            .style(style::pick);
        let mut peers_list = column![
            style::section_header("Peers"),
            row![search("Filter peers", &self.filter, Message::FilterChanged), sort]
                .spacing(8)
                .align_y(iced::Center),
        ]
        .spacing(8);
        for peer in self.visible_peers() {
            peers_list = peers_list.push(self.peer_row(peer, false));
        }
        let visible_nearby = self.visible_nearby();
        let mut nearby = column![style::section_header("Discovered nearby")].spacing(8);
        if visible_nearby.is_empty() {
            nearby = nearby.push(text("No other nodes seen on the LAN or tailnet yet").size(SMALL).style(style::muted));
        }
        for peer in visible_nearby {
            nearby = nearby.push(self.peer_row(peer, true));
        }
        let mut add = column![
            style::section_header("Add by public key"),
            field("Public key", &self.add_key, Message::KeyChanged),
            field("Address (optional)", &self.add_address, Message::AddressChanged),
        ]
        .spacing(6);
        if !self.add_key.trim().is_empty() && self.key_validation.as_ref().is_some_and(|(_, valid)| !valid) {
            add = add.push(text("Invalid public key").size(SMALL).style(text::danger));
        }
        let mut add_button = filled("Add peer");
        if self.key_validation.as_ref().is_some_and(|(key, valid)| *valid && key == self.add_key.trim()) {
            add_button = add_button.on_press(Message::AddPeer);
        }
        add = add.push(add_button);
        let content = column![
            self.node_view(),
            style::divider(),
            peers_list,
            nearby,
            add,
            style::divider(),
            self.shared_view(),
            self.folders_view(),
            style::divider(),
            self.grants_view(),
            style::divider(),
            self.received_view(),
        ]
        .spacing(12)
        .padding(12);
        scrollable(container(content).width(Length::Fill))
            .direction(style::thin_scrollbar())
            .style(style::scrollbar)
            .into()
    }

    /// This node: name, key, where the key lives, the endpoint and discovery.
    fn node_view(&self) -> Element<'_, Message> {
        let node = &self.data.node;
        let short: String = node.key.chars().take(12).collect();
        let status = |label: &'static str, value: &str, tone: style::Tone| {
            row![
                container(space().width(6.0).height(6.0)).style(style::dot(tone)),
                text(label).size(SMALL).style(style::muted).width(Length::Fixed(64.0)),
                text(value.to_owned()).size(SMALL).width(Fill),
            ]
            .spacing(8)
            .align_y(iced::Center)
        };
        let endpoint_tone = if node.endpoint_ok { style::Tone::Ok } else if node.endpoint.is_empty() { style::Tone::Off } else { style::Tone::Bad };
        let lan_tone = if node.lan_ok { style::Tone::Ok } else { style::Tone::Off };
        let tailscale_tone = if node.tailscale_ok { style::Tone::Ok } else { style::Tone::Off };
        let mut actions = row![
            flat(text("Copy key").size(SMALL)).on_press(Message::CopyKey(node.key.clone())),
            flat(text("Refresh").size(SMALL)).on_press(Message::Refresh),
        ]
        .spacing(4);
        if node.tailscale_available {
            actions = actions.push(flat(text("Scan tailnet").size(SMALL)).on_press(Message::ScanTailscale));
        }
        column![
            style::section_header("This node"),
            row![text(node.name.as_str()).size(14), text(short).size(SMALL).style(style::muted)]
                .spacing(8)
                .align_y(iced::Center),
            status("Key", &node.key_source, style::Tone::Ok),
            status("Endpoint", if node.endpoint.is_empty() { "not started" } else { &node.endpoint }, endpoint_tone),
            status("LAN", &node.lan, lan_tone),
            status("Tailscale", &node.tailscale, tailscale_tone),
            actions,
        ]
        .spacing(6)
        .into()
    }

    /// Peers' sessions this node may open, drive or fork.
    fn shared_view(&self) -> Element<'_, Message> {
        let mut list = column![style::section_header("Shared with you")].spacing(8);
        if self.data.remote_sessions.is_empty() {
            return list
                .push(text("No peer has shared a session with this node").size(SMALL).style(style::muted))
                .into();
        }
        for item in &self.data.remote_sessions {
            let mut actions = row![].spacing(4);
            let open_label = if item.can_drive { "Open and drive" } else { "Open" };
            let mut open = flat(text(open_label).size(SMALL));
            if item.online {
                open = open.on_press(Message::OpenRemote { peer: item.peer_key.clone(), session: item.session.clone() });
            }
            actions = actions.push(open);
            if item.can_fork {
                let mut fork = flat(text("Fork to this node").size(SMALL));
                if item.online {
                    fork = fork.on_press(Message::ForkRemote { peer: item.peer_key.clone(), session: item.session.clone() });
                }
                actions = actions.push(fork);
            }
            let tone = if item.online { style::Tone::Ok } else { style::Tone::Off };
            list = list.push(list_item(
                column![
                    row![
                        container(space().width(6.0).height(6.0)).style(style::dot(tone)),
                        text(item.title.as_str()).size(14).width(Fill),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                    text(format!("{} · {} · {}", item.peer_name, item.harness, item.access)).size(SMALL).style(style::muted),
                    actions,
                ]
                .spacing(4),
            ));
        }
        list.into()
    }

    /// Peers' folders this node may browse, read and (with drive) write.
    fn folders_view(&self) -> Element<'_, Message> {
        let mut list = column![style::section_header("Shared folders")].spacing(8);
        if self.data.shared_folders.is_empty() {
            return list.push(text("No peer has shared a folder with this node").size(SMALL).style(style::muted)).into();
        }
        for folder in &self.data.shared_folders {
            let mut browse = flat(row![muted(Icon::Folder, 14.0), text("Browse").size(SMALL)].spacing(6).align_y(iced::Center));
            if folder.online {
                browse = browse.on_press(Message::BrowseFolder { peer: folder.peer_key.clone(), folder: folder.path.clone() });
            }
            let tone = if folder.online { style::Tone::Ok } else { style::Tone::Off };
            list = list.push(list_item(
                column![
                    row![
                        container(space().width(6.0).height(6.0)).style(style::dot(tone)),
                        text(folder.path.as_str()).size(14).width(Fill),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                    text(format!("{} · {}", folder.peer_name, if folder.can_write { "read, write" } else { "read" }))
                        .size(SMALL)
                        .style(style::muted),
                    browse,
                ]
                .spacing(4),
            ));
        }
        list.into()
    }

    /// Every grant this node holds, from any peer.
    fn received_view(&self) -> Element<'_, Message> {
        let mut list = column![style::section_header("Received grants")].spacing(8);
        let received: Vec<&Grant> = self.data.grants.iter().filter(|g| g.direction == GrantDirection::Received).collect();
        if received.is_empty() {
            return list.push(text("No peer has granted this node anything").size(SMALL).style(style::muted)).into();
        }
        for grant in received {
            let from = self.data.peers.iter().find(|p| p.key == grant.peer_key).map_or_else(
                || grant.peer_key.chars().take(12).collect::<String>(),
                |p| p.name.clone(),
            );
            list = list.push(
                text(format!("From {from}: {} · {}", grant_summary(grant), grant.expiry_text(self.data.now)))
                    .size(SMALL)
                    .style(style::muted),
            );
        }
        list.into()
    }

    fn peer_row<'a>(&'a self, peer: &'a Peer, discovered: bool) -> Element<'a, Message> {
        let version = peer.version_relation(&self.data.local_version);
        let mut actions = row![flat(text("Copy full key").size(SMALL)).on_press(Message::CopyKey(peer.key.clone()))]
            .spacing(4);
        if discovered {
            actions = actions.push(flat(text("Add").size(SMALL)).on_press(Message::AddDiscovered(peer.key.clone())));
        } else {
            actions = actions.push(flat(text("Grants").size(SMALL)).on_press(Message::SelectPeer(peer.key.clone())));
            actions = actions.push(flat(text("Rename").size(SMALL)).on_press(Message::BeginRename(peer.key.clone())));
            actions = actions.push(
                flat(text("Remove").size(SMALL))
                    .style(style::danger_flat)
                    .on_press(Message::BeginRemove(peer.key.clone())),
            );
        }
        let mut card = column![
            row![
                container(space().width(6.0).height(6.0)).style(style::dot(presence_tone(peer.presence))),
                text(peer.name.as_str()).size(14),
                text(peer.short_key()).size(SMALL).style(style::muted),
            ]
            .spacing(8)
            .align_y(iced::Center),
            text(format!(
                "{} · {} · {} · last seen {} · {} ({})",
                peer.discovery,
                peer.presence,
                peer.transport,
                peer.last_seen_text(self.data.now),
                peer.app_version,
                version
            ))
            .size(SMALL)
            .style(style::muted),
            text(match peer.shared_protocol {
                Some(true) => "Shared protocol available",
                Some(false) => "No shared protocol",
                None => "Not contacted yet",
            })
            .size(SMALL)
            .style(style::muted),
            actions,
        ]
        .spacing(4);
        if self.pending_remove.as_deref() == Some(&peer.key) {
            card = card.push(
                row![
                    text("Remove this peer?").size(SMALL).style(style::muted),
                    flat(text("Confirm remove").size(SMALL))
                        .style(style::danger_flat)
                        .on_press(Message::ConfirmRemove),
                    flat(text("Cancel").size(SMALL)).on_press(Message::CancelRemove),
                ]
                .spacing(4)
                .align_y(iced::Center),
            );
        }
        if let Some((key, name)) = &self.rename && key == &peer.key {
            card = card.push(
                row![
                    field("Peer name", name, Message::RenameChanged),
                    flat(text("Save").size(SMALL)).on_press(Message::SaveRename),
                    flat(text("Cancel").size(SMALL)).on_press(Message::CancelRename),
                ]
                .spacing(4)
                .align_y(iced::Center),
            );
        }
        list_item(card)
    }

    fn grants_view(&self) -> Element<'_, Message> {
        let Some(key) = &self.selected_peer else {
            return text("Select a peer to manage grants").size(SMALL).style(style::muted).into();
        };
        let name = self.data.peers.iter().find(|p| &p.key == key).map_or("this peer", |p| p.name.as_str());
        let mut list = column![style::section_header(format!("Grants with {name}"))].spacing(8);
        for grant in self.data.grants.iter().filter(|g| &g.peer_key == key) {
            let mut line = row![
                text(format!(
                    "{}: {} · {} · {}",
                    grant.direction,
                    grant_summary(grant),
                    grant.expiry_text(self.data.now),
                    if grant.reshare { "re-share allowed" } else { "no re-share" }
                ))
                .size(SMALL)
                .style(style::muted)
                .width(Fill),
            ]
            .spacing(8)
            .align_y(iced::Center);
            if grant.direction == GrantDirection::Given {
                line = line.push(flat(text("Renew").size(SMALL)).on_press(Message::RenewGrant(grant.id.clone())));
                if !grant.ended {
                    line = line.push(
                        flat(text("Revoke").size(SMALL))
                            .style(style::danger_flat)
                            .on_press(Message::BeginRevoke(grant.id.clone())),
                    );
                }
            }
            list = list.push(line);
            if self.pending_revoke.as_deref() == Some(&grant.id) {
                list = list.push(
                    row![
                        text("Revoke this grant?").size(SMALL).style(style::muted),
                        flat(text("Confirm revoke").size(SMALL))
                            .style(style::danger_flat)
                            .on_press(Message::ConfirmRevoke),
                        flat(text("Cancel").size(SMALL)).on_press(Message::CancelRevoke),
                    ]
                    .spacing(4)
                    .align_y(iced::Center),
                );
            }
        }
        let mut form = column![
            style::section_header("Create grant"),
            text("Resource").size(SMALL).style(style::muted),
        ]
        .spacing(8);
        let term = self.resource_filter.trim().to_lowercase();
        let matching: Vec<&ResourceChoice> = self
            .data
            .resources
            .iter()
            .filter(|r| term.is_empty() || r.label.to_lowercase().contains(&term))
            .collect();
        if self.data.resources.len() > RESOURCE_ROWS {
            form = form.push(search("Filter sessions, prompts, hosts", &self.resource_filter, Message::ResourceFilterChanged));
        }
        let chosen_hidden = self.grant_form.resource.as_ref()
            .is_some_and(|chosen| !matching.iter().take(RESOURCE_ROWS).any(|r| r.id == chosen.id));
        if let Some(chosen) = self.grant_form.resource.as_ref().filter(|_| chosen_hidden) {
            form = form.push(choice(chosen.label.clone(), true, Message::SelectResource(chosen.id.clone())));
        }
        for resource in matching.iter().take(RESOURCE_ROWS) {
            let chosen = self.grant_form.resource.as_ref().is_some_and(|r| r.id == resource.id);
            form = form.push(choice(
                resource.label.clone(),
                chosen,
                Message::SelectResource(resource.id.clone()),
            ));
        }
        if matching.len() > RESOURCE_ROWS {
            form = form.push(
                text(format!("{} more; filter to find them", matching.len() - RESOURCE_ROWS)).size(SMALL).style(style::muted),
            );
        }
        form = form.push(
            row![
                field("Folder on this node, e.g. /home/me/shared", &self.grant_form.folder_path, Message::FolderPathChanged)
                    .on_submit(Message::UseFolder)
                    .width(Fill),
                flat(text("Use folder").size(SMALL)).on_press(Message::UseFolder),
            ]
            .spacing(8)
            .align_y(iced::Center),
        );
        if let Some(resource) = &self.grant_form.resource {
            if resource.scope == GrantScope::Prompt {
                form = form.push(
                    iced::widget::checkbox(self.grant_form.future_sessions)
                        .label("Include every future session")
                        .size(14)
                        .text_size(13)
                        .on_toggle(Message::FutureSessionsChanged),
                );
            }
            form = form.push(text("Permission").size(SMALL).style(style::muted));
            for permission in Permission::ALL {
                if permission.allowed_for(&resource.scope) {
                    form = form.push(choice(
                        permission.to_string(),
                        self.grant_form.permission == permission,
                        Message::SelectPermission(permission),
                    ));
                }
            }
            if self.grant_form.is_ssh() && !self.grant_form.ssh_confirmed {
                form = form.push(
                    text("SSH access is off until explicitly confirmed.").size(SMALL).style(style::muted),
                );
                form = form.push(flat(text("Enable SSH grant").size(SMALL)).on_press(Message::RequestSshConfirmation));
                if self.grant_form.ssh_confirmation_pending {
                    form = form.push(
                        row![
                            text("Allow this peer to request SSH access through this node?")
                                .size(SMALL)
                                .style(style::muted),
                            flat(text("Confirm SSH access").size(SMALL))
                                .style(style::danger_flat)
                                .on_press(Message::ConfirmSsh),
                            flat(text("Cancel").size(SMALL)).on_press(Message::CancelSsh),
                        ]
                        .spacing(4)
                        .align_y(iced::Center),
                    );
                }
            }
        }
        form = form.push(
            row![
                text("Lasts (hours)").size(SMALL).style(style::muted),
                field("hours", &self.grant_form.ttl_hours, Message::TtlChanged).width(Length::Fixed(80.0)),
            ]
            .spacing(8)
            .align_y(iced::Center),
        );
        form = form.push(
            iced::widget::checkbox(self.grant_form.reshare)
                .label("Allow re-share")
                .size(14)
                .text_size(13)
                .on_toggle(Message::ReshareChanged),
        );
        let mut submit = filled("Create grant");
        if self.grant_form.build(key).is_ok() {
            submit = submit.on_press(Message::SubmitGrant);
        }
        list.push(style::divider()).push(form.push(submit)).into()
    }
}

/// Text-first button: no fill until hovered.
fn flat<'a>(content: impl Into<Element<'a, Message>>) -> button::Button<'a, Message> {
    button(content).padding([4.0, 8.0]).style(style::flat)
}

/// The one filled action of a form.
fn filled(label: &str) -> button::Button<'_, Message> {
    button(text(label).size(SMALL)).padding([4.0, 10.0]).style(button::primary)
}

/// A full-width list choice; the chosen one keeps a raised background.
fn choice<'a>(label: impl Into<String>, chosen: bool, message: Message) -> button::Button<'a, Message> {
    let label: String = label.into();
    button(text(label).size(SMALL))
        .width(Fill)
        .padding([4.0, 8.0])
        .style(if chosen { style::selected } else { style::flat })
        .on_press(message)
}

/// A raised text field, styled like every other input in the panel.
fn field<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> text_input::TextInput<'a, Message> {
    text_input(placeholder, value)
        .on_input(on_input)
        .size(14)
        .style(style::input)
        .padding([6.0, 8.0])
}

/// The filter field: a raised block with a search icon.
fn search<'a>(
    placeholder: &'a str,
    value: &'a str,
    on_input: impl Fn(String) -> Message + 'a,
) -> Element<'a, Message> {
    container(
        row![
            muted(Icon::Search, 14.0),
            text_input(placeholder, value)
                .on_input(on_input)
                .size(14)
                .style(style::bare)
                .padding(0.0)
                .width(Fill),
        ]
        .spacing(8)
        .align_y(iced::Center),
    )
    .padding([6.0, 10.0])
    .width(Fill)
    .style(style::card)
    .into()
}

/// A row of a list, closed by a hairline.
fn list_item<'a>(content: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    column![container(content).padding([10.0, 0.0]).width(Fill), style::divider()].into()
}

fn presence_tone(presence: Presence) -> style::Tone {
    match presence {
        Presence::Online => style::Tone::Ok,
        Presence::Offline => style::Tone::Off,
    }
}
