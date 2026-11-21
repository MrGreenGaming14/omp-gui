//! The panel: this node, peers and nearby nodes, adding by key, what peers share, the selected
//! peer's grants with the grant form, and grants received.

use super::state::{Confirm, Local, Message, State};
use super::{Event, GrantDirection, GrantScope, Peer, PeerSort, Permission, Presence, ResourceChoice, grant_summary};
use crate::{action, check, dot, field, pick, section};
use iced::widget::{Column, column, row, rule};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::Icon;
use loa_ui_kit::style::{self, ButtonKind, Tone};
use loa_ui_kit::theme::{LayoutRole, SpaceRole, TextRole, sp, width};
use loa_ui_kit::widgets::{Msg, label, menu_row};

/// Resource choices shown at once; the filter narrows longer lists.
const RESOURCE_ROWS: usize = 8;

fn local(m: Local) -> Option<Message> { Some(Msg::Local(m)) }
fn emit(e: Event) -> Option<Message> { Some(Msg::Emit(e)) }
fn caption<'a>(s: impl iced::widget::text::IntoFragment<'a>) -> iced::widget::Text<'a> { label(s, TextRole::Caption, Tone::Muted) }
fn online(on: bool) -> Tone { if on { Tone::Ok } else { Tone::Faint } }

/// A list entry: a status light and title over its other lines.
fn item<'a>(tone: Tone, title: impl iced::widget::text::IntoFragment<'a>, lines: Column<'a, Message>) -> Element<'a, Message> {
    let head = row![dot(tone), label(title, TextRole::Body, Tone::Normal)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
    column![head, lines.spacing(sp(SpaceRole::Xs))].spacing(sp(SpaceRole::Xs)).into()
}

/// A question with a confirming danger button and Cancel.
fn confirm<'a>(question: &'a str, yes: &'a str) -> Element<'a, Message> {
    row![caption(question), action(yes, ButtonKind::Danger, local(Local::Confirm)), action("Cancel", ButtonKind::Ghost, local(Local::Cancel))].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center).into()
}

impl State {
    pub fn view(&self) -> Element<'_, Message> {
        let sort = pick(PeerSort::ALL, Some(self.sort), "", |s| Msg::Local(Local::Sort(s)));
        let filter = row![field("Filter peers", &self.filter, |f| Msg::Local(Local::Filter(f))).width(Fill), sort].spacing(sp(SpaceRole::S));
        let peers = column![filter].extend(self.visible_peers().into_iter().map(|p| self.peer_row(p, false)));
        let nearby = self.visible_nearby();
        let nearby = Column::with_children(nearby.iter().map(|p| self.peer_row(p, true))).push(nearby.is_empty().then(|| caption("No other nodes seen on the LAN or tailnet yet")));
        let invalid = !self.add_key.trim().is_empty() && self.key_validation.as_ref().is_some_and(|(_, valid)| !valid);
        let add = column![field("Public key", &self.add_key, |k| Msg::Local(Local::Key(k))), field("Address (optional)", &self.add_address, |a| Msg::Local(Local::Address(a)))];
        let add = add.push(invalid.then(|| label("Invalid public key", TextRole::Caption, Tone::Danger))).push(action("Add peer", ButtonKind::Primary, self.key_valid().then_some(Msg::Local(Local::AddPeer))));
        let gap = sp(SpaceRole::S);
        let divider = || rule::horizontal(1).style(style::rule);
        let content = column![
            self.node_view(),
            divider(),
            section("Peers", peers.spacing(gap)),
            section("Discovered nearby", nearby.spacing(gap)),
            section("Add by public key", add.spacing(gap)),
            divider(),
            self.shared_view(),
            self.folders_view(),
            divider(),
            self.grants_view(),
            divider(),
            self.received_view(),
        ];
        crate::scroll(content.spacing(sp(SpaceRole::M)))
    }

    /// This node: name, key, where the key lives, the endpoint and discovery.
    fn node_view(&self) -> Element<'_, Message> {
        let node = &self.data.node;
        let status = |name: &'static str, value: &str, tone: Tone| row![dot(tone), caption(name).width(width(LayoutRole::LabelColumn) / 2.0), label(value.to_owned(), TextRole::Caption, Tone::Normal)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let endpoint_tone = if node.endpoint_ok { Tone::Ok } else if node.endpoint.is_empty() { Tone::Faint } else { Tone::Danger };
        let actions = row![action("Copy key", ButtonKind::Ghost, emit(Event::CopyKey(node.key.clone()))), action("Refresh", ButtonKind::Ghost, emit(Event::Refresh))];
        let actions = actions.push(node.tailscale_available.then(|| action("Scan tailnet", ButtonKind::Ghost, emit(Event::ScanTailscale))));
        let name = row![label(node.name.as_str(), TextRole::Body, Tone::Normal), caption(node.key.chars().take(12).collect::<String>())].spacing(sp(SpaceRole::S));
        let lines = column![
            name,
            status("Key", &node.key_source, Tone::Ok),
            status("Endpoint", if node.endpoint.is_empty() { "not started" } else { node.endpoint.as_str() }, endpoint_tone),
            status("LAN", &node.lan, online(node.lan_ok)),
            status("Tailscale", &node.tailscale, online(node.tailscale_ok)),
            actions.spacing(sp(SpaceRole::Xs)),
        ];
        section("This node", lines.spacing(sp(SpaceRole::Xs)))
    }

    fn peer_row<'a>(&'a self, peer: &'a Peer, nearby: bool) -> Element<'a, Message> {
        let key = || peer.key.clone();
        let mut actions = row![action("Copy full key", ButtonKind::Ghost, emit(Event::CopyKey(key())))].spacing(sp(SpaceRole::Xs));
        actions = if nearby {
            actions.push(action("Add", ButtonKind::Ghost, emit(Event::AddDiscovered(key()))))
        } else {
            let remove = action("Remove", ButtonKind::Danger, local(Local::Ask(Confirm::RemovePeer(key()))));
            actions.push(action("Grants", ButtonKind::Ghost, local(Local::SelectPeer(key())))).push(action("Rename", ButtonKind::Ghost, local(Local::BeginRename(key())))).push(remove)
        };
        let version = peer.version_relation(&self.data.local_version);
        let seen = format!("{} · {} · {} · last seen {} · {} ({version})", peer.discovery, peer.presence, peer.transport, peer.last_seen_text(self.data.now), peer.app_version);
        let protocol = match peer.shared_protocol { Some(true) => "Shared protocol available", Some(false) => "No shared protocol", None => "Not contacted yet" };
        let removing = (self.confirm == Some(Confirm::RemovePeer(peer.key.clone()))).then(|| confirm("Remove this peer?", "Confirm remove"));
        let renaming = self.rename.as_ref().filter(|(k, _)| *k == peer.key).map(|(_, name)| {
            let field = field("Peer name", name, |n| Msg::Local(Local::RenameText(n))).on_submit(Msg::Local(Local::SaveRename));
            row![field, action("Save", ButtonKind::Ghost, local(Local::SaveRename)), action("Cancel", ButtonKind::Ghost, local(Local::Cancel))].spacing(sp(SpaceRole::Xs)).align_y(Alignment::Center)
        });
        let title = format!("{}  {}", peer.name, peer.short_key());
        item(online(peer.presence == Presence::Online), title, column![caption(seen), caption(protocol), actions, removing, renaming])
    }

    /// Peers' sessions this node may open, drive or fork.
    fn shared_view(&self) -> Element<'_, Message> {
        let items = self.data.remote_sessions.iter().map(|r| {
            let at = |e: Event| r.online.then_some(Msg::Emit(e));
            let (peer, session) = (r.peer_key.clone(), r.session.clone());
            let open = action(if r.can_drive { "Open and drive" } else { "Open" }, ButtonKind::Ghost, at(Event::OpenRemote { peer: peer.clone(), session: session.clone() }));
            let fork = r.can_fork.then(|| action("Fork to this node", ButtonKind::Ghost, at(Event::ForkRemote { peer, session })));
            item(online(r.online), r.title.as_str(), column![caption(format!("{} · {} · {}", r.peer_name, r.harness, r.access)), row![open, fork].spacing(sp(SpaceRole::Xs))])
        });
        let empty = self.data.remote_sessions.is_empty().then(|| caption("No peer has shared a session with this node"));
        section("Shared with you", Column::with_children(items).push(empty).spacing(sp(SpaceRole::S)))
    }

    /// Peers' folders this node may browse, read and (with drive) write.
    fn folders_view(&self) -> Element<'_, Message> {
        let items = self.data.shared_folders.iter().map(|f| {
            let browse = f.online.then(|| Msg::Emit(Event::BrowseFolder { peer: f.peer_key.clone(), folder: f.path.clone() }));
            let browse = iced::widget::button(row![loa_ui_kit::icons::tinted(Icon::Folder, TextRole::Caption, Tone::Muted), caption("Browse")].spacing(sp(SpaceRole::Xs))).style(style::button(ButtonKind::Ghost)).on_press_maybe(browse);
            item(online(f.online), f.path.as_str(), column![caption(format!("{} · {}", f.peer_name, if f.can_write { "read, write" } else { "read" })), browse])
        });
        let empty = self.data.shared_folders.is_empty().then(|| caption("No peer has shared a folder with this node"));
        section("Shared folders", Column::with_children(items).push(empty).spacing(sp(SpaceRole::S)))
    }

    /// Every grant this node holds, from any peer.
    fn received_view(&self) -> Element<'_, Message> {
        let received = self.data.grants.iter().filter(|g| g.direction == GrantDirection::Received).map(|g| {
            let from = self.data.peers.iter().find(|p| p.key == g.peer_key).map_or_else(|| g.peer_key.chars().take(12).collect(), |p| p.name.clone());
            caption(format!("From {from}: {} · {}", grant_summary(g), g.expiry_text(self.data.now))).into()
        });
        let empty = !self.data.grants.iter().any(|g| g.direction == GrantDirection::Received);
        section("Received grants", Column::with_children(received).push(empty.then(|| caption("No peer has granted this node anything"))).spacing(sp(SpaceRole::Xs)))
    }

    fn grants_view(&self) -> Element<'_, Message> {
        let Some(key) = &self.selected_peer else { return caption("Select a peer to manage grants").into() };
        let name = self.data.peers.iter().find(|p| &p.key == key).map_or("this peer", |p| p.name.as_str());
        let grants = self.data.grants.iter().filter(|g| &g.peer_key == key).map(|g| {
            let reshare = if g.reshare { "re-share allowed" } else { "no re-share" };
            let line = row![caption(format!("{}: {} · {} · {reshare}", g.direction, grant_summary(g), g.expiry_text(self.data.now))).width(Fill)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
            let given = g.direction == GrantDirection::Given;
            let line = line.push(given.then(|| action("Renew", ButtonKind::Ghost, emit(Event::RenewGrant(g.id.clone()))))).push((given && !g.ended).then(|| action("Revoke", ButtonKind::Danger, local(Local::Ask(Confirm::RevokeGrant(g.id.clone()))))));
            let revoking = (self.confirm == Some(Confirm::RevokeGrant(g.id.clone()))).then(|| confirm("Revoke this grant?", "Confirm revoke"));
            column![line, revoking].into()
        });
        let grants = Column::with_children(grants).spacing(sp(SpaceRole::Xs));
        column![section(format!("Grants with {name}"), grants), section("Create grant", self.form_view(key))].spacing(sp(SpaceRole::M)).into()
    }

    fn form_view<'a>(&'a self, key: &str) -> Column<'a, Message> {
        let form = &self.grant_form;
        let term = self.resource_filter.trim().to_lowercase();
        let matching: Vec<&ResourceChoice> = self.data.resources.iter().filter(|r| r.label.to_lowercase().contains(&term)).collect();
        let choice = |r: &ResourceChoice| menu_row(None, r.label.clone(), form.resource.as_ref().is_some_and(|c| c.id == r.id), Msg::Local(Local::SelectResource(r.id.clone())));
        let filter = (self.data.resources.len() > RESOURCE_ROWS).then(|| field("Filter sessions, prompts, hosts", &self.resource_filter, |f| Msg::Local(Local::ResourceFilter(f))));
        let hidden = form.resource.as_ref().filter(|c| !matching.iter().take(RESOURCE_ROWS).any(|r| r.id == c.id)).map(choice);
        let more = (matching.len() > RESOURCE_ROWS).then(|| caption(format!("{} more; filter to find them", matching.len() - RESOURCE_ROWS)));
        let folder = field("Folder on this node, e.g. /home/me/shared", &form.folder_path, |p| Msg::Local(Local::FolderPath(p))).on_submit(Msg::Local(Local::UseFolder));
        let folder = row![folder.width(Fill), action("Use folder", ButtonKind::Ghost, local(Local::UseFolder))].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let mut col = column![caption("Resource"), filter, hidden].extend(matching.iter().take(RESOURCE_ROWS).map(|r| choice(r))).push(more).push(folder);
        if let Some(resource) = &form.resource {
            col = col.push((resource.scope == GrantScope::Prompt).then(|| check(form.future_sessions, "Include every future session", |on| Msg::Local(Local::FutureSessions(on)))));
            let permissions = Permission::ALL.iter().filter(|p| p.allowed_for(&resource.scope)).map(|&p| menu_row(None, p.as_str(), form.permission == p, Msg::Local(Local::Permission(p))));
            col = col.push(caption("Permission")).extend(permissions);
            if form.is_ssh() && !form.ssh_confirmed {
                col = col.push(caption("SSH access is off until explicitly confirmed.")).push(action("Enable SSH grant", ButtonKind::Ghost, local(Local::Ask(Confirm::Ssh))));
                col = col.push((self.confirm == Some(Confirm::Ssh)).then(|| confirm("Allow this peer to request SSH access through this node?", "Confirm SSH access")));
            }
        }
        let ttl = row![caption("Lasts (hours)"), field("hours", &form.ttl_hours, |t| Msg::Local(Local::Ttl(t))).width(width(LayoutRole::LabelColumn) / 2.0)].spacing(sp(SpaceRole::S)).align_y(Alignment::Center);
        let submit = action("Create grant", ButtonKind::Primary, form.build(key).is_ok().then_some(Msg::Local(Local::SubmitGrant)));
        col.push(ttl).push(check(form.reshare, "Allow re-share", |on| Msg::Local(Local::Reshare(on)))).push(submit).spacing(sp(SpaceRole::S))
    }
}
