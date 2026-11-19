//! Peer discovery and capability-grant screens driven by app-owned data.

mod grants;
mod peers;

pub use grants::{
    Grant, GrantDirection, GrantForm, GrantFormError, GrantInput, GrantResource, GrantScope,
    Permission, ResourceChoice, grant_summary,
};
pub use peers::{Discovery, Peer, PeerSort, Presence, Transport, VersionRelation};

use iced::widget::{button, column, container, row, scrollable, text, text_input};
use iced::{Element, Length, Task};

/// App-supplied snapshot consumed by the two screens.
#[derive(Debug, Clone, Default)]
pub struct Data {
    pub peers: Vec<Peer>,
    pub nearby: Vec<Peer>,
    pub grants: Vec<Grant>,
    pub resources: Vec<ResourceChoice>,
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
}

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
        if self.grant_form.resource.as_ref().is_some_and(|resource| !data.resources.iter().any(|r| r == resource)) {
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
            Message::CopyKey(key) => self.data.peers.iter().chain(&self.data.nearby).any(|p| p.key == key).then_some(Event::CopyKey(key)),
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
                None
            }
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
        };
        (Task::none(), event)
    }

    /// Render the peers list and the selected peer's grant management controls.
    pub fn view(&self) -> Element<'_, Message> {
        let sort = iced::widget::pick_list(PeerSort::ALL, Some(self.sort), Message::SortChanged);
        let mut peers_list = column![
            text("Peers").size(24),
            row![text_input("Filter peers", &self.filter).on_input(Message::FilterChanged), sort].spacing(8),
        ].spacing(10);
        for peer in self.visible_peers() {
            peers_list = peers_list.push(self.peer_row(peer, false));
        }
        let mut nearby = column![text("Discovered nearby").size(20)].spacing(6);
        for peer in self.visible_nearby() {
            nearby = nearby.push(self.peer_row(peer, true));
        }
        let mut add = column![
            text("Add by public key"),
            text_input("Public key", &self.add_key).on_input(Message::KeyChanged),
            text_input("Address (optional)", &self.add_address).on_input(Message::AddressChanged),
        ].spacing(6);
        if !self.add_key.trim().is_empty() && self.key_validation.as_ref().is_some_and(|(_, valid)| !valid) {
            add = add.push(text("Invalid public key"));
        }
        let mut add_button = button("Add peer");
        if self.key_validation.as_ref().is_some_and(|(key, valid)| *valid && key == self.add_key.trim()) {
            add_button = add_button.on_press(Message::AddPeer);
        }
        add = add.push(add_button);
        let content = column![peers_list, nearby, add, self.grants_view()].spacing(20).padding(16);
        scrollable(container(content).width(Length::Fill)).into()
    }

    fn peer_row<'a>(&'a self, peer: &'a Peer, discovered: bool) -> Element<'a, Message> {
        let version = peer.version_relation(&self.data.local_version);
        let mut actions = row![
            button("Copy full key").on_press(Message::CopyKey(peer.key.clone())),
        ].spacing(6);
        if discovered {
            actions = actions.push(button("Add").on_press(Message::AddDiscovered(peer.key.clone())));
        } else {
            actions = actions.push(button("Grants").on_press(Message::SelectPeer(peer.key.clone())));
            actions = actions.push(button("Rename").on_press(Message::BeginRename(peer.key.clone())));
            actions = actions.push(button("Remove").on_press(Message::BeginRemove(peer.key.clone())));
        }
        let mut card = column![
            text(format!("{} · {}", peer.name, peer.short_key())),
            text(format!("{} · {} · {} · last seen {} · {} ({})",
                peer.discovery, peer.presence, peer.transport, peer.last_seen_text(self.data.now),
                peer.app_version, version)),
            text(if peer.shared_protocol { "Shared protocol available" } else { "No shared protocol" }),
            actions,
        ].spacing(4);
        if self.pending_remove.as_deref() == Some(&peer.key) {
            card = card.push(row![
                text("Remove this peer?"),
                button("Confirm remove").on_press(Message::ConfirmRemove),
                button("Cancel").on_press(Message::CancelRemove)
            ].spacing(6));
        }
        if let Some((key, name)) = &self.rename && key == &peer.key {
            card = card.push(row![
                text_input("Peer name", name).on_input(Message::RenameChanged),
                button("Save").on_press(Message::SaveRename),
                button("Cancel").on_press(Message::CancelRename),
            ].spacing(6));
        }
        container(card).padding(8).width(Length::Fill).into()
    }

    fn grants_view(&self) -> Element<'_, Message> {
        let Some(key) = &self.selected_peer else { return text("Select a peer to manage grants").into() };
        let mut list = column![text("Grants").size(24)].spacing(8);
        for grant in self.data.grants.iter().filter(|g| &g.peer_key == key) {
            let mut line = row![
                text(format!("{}: {} · {} · {}",
                    grant.direction, grant_summary(grant), grant.expiry_text(self.data.now),
                    if grant.reshare { "re-share allowed" } else { "no re-share" })),
            ].spacing(8);
            if grant.direction == GrantDirection::Given {
                line = line.push(button("Revoke").on_press(Message::BeginRevoke(grant.id.clone())));
            }
            list = list.push(line);
            if self.pending_revoke.as_deref() == Some(&grant.id) {
                list = list.push(row![
                    text("Revoke this grant?"),
                    button("Confirm revoke").on_press(Message::ConfirmRevoke),
                    button("Cancel").on_press(Message::CancelRevoke)
                ].spacing(6));
            }
        }
        let mut form = column![text("Create grant").size(20)].spacing(6);
        for resource in &self.data.resources {
            let selected = self.grant_form.resource.as_ref().is_some_and(|r| r.id == resource.id);
            form = form.push(button(text(format!("{} {}", if selected { "●" } else { "○" }, resource.label)))
                .on_press(Message::SelectResource(resource.id.clone())));
        }
        if let Some(resource) = &self.grant_form.resource {
            if resource.scope == GrantScope::Prompt {
                form = form.push(iced::widget::checkbox(self.grant_form.future_sessions)
                    .label("Include every future session")
                    .on_toggle(Message::FutureSessionsChanged));
            }
            for permission in Permission::ALL {
                if permission.allowed_for(&resource.scope) {
                    form = form.push(button(text(format!("{} {permission}", if self.grant_form.permission == permission { "●" } else { "○" })))
                        .on_press(Message::SelectPermission(permission)));
                }
            }
            if self.grant_form.is_ssh() && !self.grant_form.ssh_confirmed {
                form = form.push(text("SSH access is off until explicitly confirmed."));
                form = form.push(button("Enable SSH grant").on_press(Message::RequestSshConfirmation));
                if self.grant_form.ssh_confirmation_pending {
                    form = form.push(row![
                        text("Allow this peer to request SSH access through this node?"),
                        button("Confirm SSH access").on_press(Message::ConfirmSsh),
                        button("Cancel").on_press(Message::CancelSsh)
                    ].spacing(6));
                }
            }
        }
        form = form.push(text_input("TTL (hours)", &self.grant_form.ttl_hours).on_input(Message::TtlChanged));
        form = form.push(iced::widget::checkbox(self.grant_form.reshare).label("Allow re-share").on_toggle(Message::ReshareChanged));
        let mut submit = button("Create grant");
        if self.grant_form.build(key).is_ok() { submit = submit.on_press(Message::SubmitGrant); }
        list.push(form.push(submit)).into()
    }
}
