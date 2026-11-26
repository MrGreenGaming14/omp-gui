//! Per-parent child subtabs, live transcript polling and subagent messaging.

use std::collections::HashMap;
use std::path::PathBuf;

use iced::widget::{button, column, container, row, rule, scrollable, space, text_input};
use iced::{Alignment, Element, Fill, Task, Theme};
use loa_bus::Listing;
use loa_contracts::harness::{AdapterError, Feature, SubagentTranscript};
use loa_contracts::session::SessionSummary;
use loa_core::{Dom, ElementKind, JobKind, JobStatus, MemorySink, NodeId, attr};
use loa_runtime::session::Session;
use loa_ui_kit::icons::tinted;
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, sp, text_size as ts};
use loa_ui_kit::widgets::{Tab, badge, card, label, tab_strip, text_button};
use loa_ui_session::transcript as chat;
use loa_view::session::transcript as chat_intent;
use loa_view::theme::Icon;

use super::{Message, State, capabilities};
use crate::area::{Ctx, Request};

/// Outcome error of `HarnessSession::message_subagent`, keeping addressability distinct.
#[derive(Clone, Debug)]
pub enum SendError {
    NotAddressable(String),
    Other(String),
}

/// One open child subtab of a parent conversation.
pub(crate) struct ChildTab {
    pub(crate) id: String,
    /// Distinguishes this tab from an earlier one of the same subagent that was closed, so a
    /// read or send started by that one never lands here.
    pub(crate) generation: u64,
    pub(crate) title: String,
    pub(crate) tab_label: String,
    pub(crate) status: Option<JobStatus>,
    pub(crate) dom: Option<Dom>,
    pub(crate) transcript: chat::State,
    pub(crate) addressable: bool,
    /// A send result arrived while a read was in flight: that read started before it, so its
    /// `addressable` is out of date and the follow-up read decides.
    pub(crate) addressable_stale: bool,
    pub(crate) loading: bool,
    pub(crate) refetch_needed: bool,
    pub(crate) fetch_error: Option<String>,
    pub(crate) draft: String,
    pub(crate) sending: bool,
    pub(crate) send_error: Option<String>,
}

impl ChildTab {
    fn new(id: String, generation: u64, title: String, status: Option<JobStatus>, theme: &Theme) -> Self {
        let mut transcript = chat::State::new(theme);
        transcript.set_read_only(true);
        Self {
            tab_label: format!("{title} · {id}"),
            id,
            generation,
            title,
            status,
            dom: None,
            transcript,
            addressable: false,
            addressable_stale: false,
            loading: false,
            refetch_needed: false,
            fetch_error: None,
            draft: String::new(),
            sending: false,
            send_error: None,
        }
    }

    /// Whether the subagent may still take messages: the harness said so, or its job still runs
    /// (a first read that failed or hasn't arrived yet).
    fn live(&self) -> bool {
        self.addressable || self.status == Some(JobStatus::Running)
    }
}

/// Open child subtabs and active selection for one parent session.
#[derive(Default)]
pub(crate) struct ParentTabs {
    pub(crate) order: Vec<String>,
    pub(crate) children: HashMap<String, ChildTab>,
    pub(crate) selected: Option<String>,
}

/// Child subtabs keyed by parent session id.
#[derive(Default)]
pub(crate) struct SubagentTabs {
    pub(crate) parents: HashMap<String, ParentTabs>,
    next_generation: u64,
}

impl SubagentTabs {
    pub(crate) fn open(&mut self, parent: &str, child: String, title: String, status: Option<JobStatus>, theme: &Theme) {
        let entry = self.parents.entry(parent.to_owned()).or_default();
        if !entry.order.iter().any(|id| id == &child) {
            entry.order.push(child.clone());
        }
        match entry.children.get_mut(&child) {
            Some(tab) => {
                tab.title.clone_from(&title);
                tab.tab_label = format!("{title} · {}", tab.id);
                if status.is_some() {
                    tab.status = status;
                }
            }
            None => {
                self.next_generation += 1;
                entry.children.insert(child.clone(), ChildTab::new(child.clone(), self.next_generation, title, status, theme));
            }
        }
        entry.selected = Some(child);
    }

    pub(crate) fn select(&mut self, parent: &str, child: Option<String>) {
        if let Some(entry) = self.parents.get_mut(parent) {
            entry.selected = child.filter(|id| entry.children.contains_key(id));
        }
    }

    pub(crate) fn close(&mut self, parent: &str, child: &str) {
        let Some(entry) = self.parents.get_mut(parent) else { return };
        entry.order.retain(|id| id != child);
        entry.children.remove(child);
        if entry.selected.as_deref() == Some(child) {
            entry.selected = None;
        }
    }

    pub(crate) fn remove_parent(&mut self, parent: &str) {
        self.parents.remove(parent);
    }

    pub(crate) fn selected_of(&self, parent: &str) -> Option<&str> {
        self.parents.get(parent)?.selected.as_deref()
    }

    /// The selected child of `parent` when it may still take messages, so it is worth polling.
    pub(crate) fn selected_live(&self, parent: &str) -> Option<String> {
        let entry = self.parents.get(parent)?;
        let selected = entry.selected.as_deref()?;
        entry.children.get(selected).filter(|child| child.live()).map(|child| child.id.clone())
    }

    /// The open tab of `child` under `parent`, if it is still the tab of `generation`.
    fn current(&mut self, parent: &str, child: &str, generation: u64) -> Option<&mut ChildTab> {
        self.parents.get_mut(parent)?.children.get_mut(child).filter(|tab| tab.generation == generation)
    }

    pub(crate) fn set_theme(&mut self, theme: &Theme) {
        for entry in self.parents.values_mut() {
            for child in entry.children.values_mut() {
                child.transcript.set_theme(theme);
            }
        }
    }
}

impl State {
    pub(crate) fn set_theme(&mut self, theme: &Theme) {
        self.subagents.set_theme(theme);
    }

    pub(super) fn sync_active_parent(&mut self, ctx: &Ctx) {
        let active = ctx.runtime.sessions().active();
        self.active_parent = active.map(|session| session.id().to_string());
        self.active_subagent_messages = active.is_some_and(|session| {
            session.harness().is_some_and(|harness| capabilities(ctx, harness.harness()).has(Feature::SubagentMessages))
        });
    }

    pub(super) fn should_poll_subagent(&self) -> bool {
        self.active_subagent_messages
            && self
                .active_parent
                .as_deref()
                .is_some_and(|parent| self.subagents.selected_live(parent).is_some())
    }

    pub(super) fn refresh_selected_subagent_on_activate(&mut self, ctx: &Ctx) -> Task<Message> {
        let Some(parent_id) = ctx.runtime.sessions().active_id().map(ToString::to_string) else {
            return Task::none();
        };
        let Some(child_id) = self.subagents.selected_of(&parent_id).map(str::to_owned) else {
            return Task::none();
        };
        let needs_fetch = self
            .subagents
            .parents
            .get(&parent_id)
            .and_then(|entry| entry.children.get(&child_id))
            .is_some_and(|child| child.dom.is_none() || child.live());
        if needs_fetch {
            self.fetch_child_transcript(ctx, &parent_id, &child_id)
        } else {
            Task::none()
        }
    }

    pub(super) fn open_child_subtab(&mut self, ctx: &Ctx, child_id: String) -> Task<Message> {
        let Some(parent_id) = ctx.runtime.sessions().active_id().map(ToString::to_string) else {
            return Task::none();
        };
        let (title, status) = self
            .agents
            .jobs()
            .find(|job| job.id() == Some(child_id.as_str()))
            .map_or_else(|| (child_id.clone(), None), |job| (job.label().to_owned(), Some(job.status())));
        self.subagents.open(&parent_id, child_id.clone(), title, status, &ctx.shell.theme());
        self.fetch_child_transcript(ctx, &parent_id, &child_id)
    }

    pub(super) fn select_child_subtab(&mut self, ctx: &Ctx, child: Option<String>) -> Task<Message> {
        let Some(parent_id) = ctx.runtime.sessions().active_id().map(ToString::to_string) else {
            return Task::none();
        };
        self.subagents.select(&parent_id, child.clone());
        let Some(child_id) = child else { return Task::none() };
        let needs_fetch = self
            .subagents
            .parents
            .get(&parent_id)
            .and_then(|entry| entry.children.get(&child_id))
            .is_some_and(|tab| tab.dom.is_none() || tab.live());
        if needs_fetch {
            self.fetch_child_transcript(ctx, &parent_id, &child_id)
        } else {
            Task::none()
        }
    }

    pub(super) fn close_child_subtab(&mut self, ctx: &Ctx, child: String) {
        if let Some(parent_id) = ctx.runtime.sessions().active_id() {
            self.subagents.close(parent_id.as_str(), &child);
        }
    }

    pub(super) fn poll_selected_subagent(&mut self, ctx: &Ctx) -> Task<Message> {
        let Some(parent_id) = ctx.runtime.sessions().active_id().map(ToString::to_string) else {
            return Task::none();
        };
        let Some(child_id) = self.subagents.selected_live(&parent_id) else {
            return Task::none();
        };
        self.fetch_child_transcript(ctx, &parent_id, &child_id)
    }

    pub(super) fn fetch_child_transcript(&mut self, ctx: &Ctx, parent_id: &str, child_id: &str) -> Task<Message> {
        let Some(harness) = ctx.runtime.sessions().get(parent_id).and_then(|session| session.harness().cloned()) else {
            return Task::none();
        };
        if !capabilities(ctx, harness.harness()).has(Feature::SubagentMessages) {
            return Task::none();
        }
        let Some(child) = self.subagents.parents.get_mut(parent_id).and_then(|entry| entry.children.get_mut(child_id)) else {
            return Task::none();
        };
        if child.loading {
            child.refetch_needed = true;
            return Task::none();
        }
        child.loading = true;
        child.refetch_needed = false;
        let generation = child.generation;
        let parent = parent_id.to_owned();
        let child_key = child_id.to_owned();
        let id_arg = child_key.clone();
        Task::perform(
            async move { harness.subagent_transcript(&id_arg).await.map_err(|error| error.to_string()) },
            move |result| Message::SubagentTranscriptLoaded { parent, child: child_key, generation, result },
        )
    }

    pub(super) fn on_transcript_loaded(&mut self, ctx: &Ctx, parent: String, child: String, generation: u64, result: Result<SubagentTranscript, String>) -> Task<Message> {
        let Some(entry) = self.subagents.current(&parent, &child, generation) else {
            return Task::none();
        };
        entry.loading = false;
        let refetch = std::mem::take(&mut entry.refetch_needed);
        match result {
            Ok(transcript) => {
                entry.fetch_error = None;
                if !std::mem::take(&mut entry.addressable_stale) {
                    entry.addressable = transcript.addressable;
                }
                if entry.dom.as_ref() != Some(&transcript.dom) {
                    entry.transcript.load(&transcript.dom);
                    entry.dom = Some(transcript.dom);
                }
            }
            Err(error) => {
                entry.fetch_error = Some(error);
            }
        }
        if refetch {
            return self.fetch_child_transcript(ctx, &parent, &child);
        }
        Task::none()
    }

    pub(super) fn set_child_draft(&mut self, ctx: &Ctx, parent: &str, child: &str, text: String) {
        if !self.can_message_child(ctx, parent, child) {
            return;
        }
        if let Some(entry) = self.subagents.parents.get_mut(parent).and_then(|tabs| tabs.children.get_mut(child)) {
            entry.draft = text;
        }
    }

    pub(super) fn send_child_message(&mut self, ctx: &Ctx, parent_id: String, child_id: String) -> Task<Message> {
        let Some(harness) = ctx.runtime.sessions().get(&parent_id).and_then(|session| session.harness().cloned()) else {
            return Task::none();
        };
        if !capabilities(ctx, harness.harness()).has(Feature::SubagentMessages) {
            return Task::none();
        }
        let Some(entry) = self.subagents.parents.get_mut(&parent_id).and_then(|tabs| tabs.children.get_mut(&child_id)) else {
            return Task::none();
        };
        if !entry.addressable || entry.sending || entry.draft.trim().is_empty() {
            return Task::none();
        }
        let text = std::mem::take(&mut entry.draft);
        entry.sending = true;
        entry.send_error = None;
        let generation = entry.generation;
        let sent = text.clone();
        let child_arg = child_id.clone();
        Task::perform(
            async move {
                harness.message_subagent(&child_arg, text).await.map_err(|error| match error {
                    AdapterError::NotAddressable(message) => SendError::NotAddressable(message),
                    other => SendError::Other(other.to_string()),
                })
            },
            move |result| Message::SubagentMessageSent { parent: parent_id, child: child_id, generation, text: sent, result },
        )
    }

    pub(super) fn on_message_sent(&mut self, ctx: &Ctx, parent: String, child: String, generation: u64, text: String, result: Result<(), SendError>) -> Task<Message> {
        let Some(entry) = self.subagents.current(&parent, &child, generation) else {
            return Task::none();
        };
        entry.sending = false;
        entry.addressable_stale = entry.loading;
        match result {
            Ok(()) => entry.send_error = None,
            Err(error) => {
                if entry.draft.trim().is_empty() {
                    entry.draft = text;
                } else {
                    entry.draft = format!("{text}\n{}", entry.draft);
                }
                let message = match error {
                    SendError::NotAddressable(message) => {
                        entry.addressable = false;
                        message
                    }
                    SendError::Other(message) => message,
                };
                entry.send_error = Some(message);
            }
        }
        // Shows the delivered message, or the final state of a subagent that stopped taking them.
        self.fetch_child_transcript(ctx, &parent, &child)
    }

    pub(super) fn update_child_chat(&mut self, ctx: &mut Ctx, parent: String, child: String, message: chat::Message) -> Task<Message> {
        let Some(entry) = self.subagents.parents.get_mut(&parent).and_then(|tabs| tabs.children.get_mut(&child)) else {
            return Task::none();
        };
        let (task, event) = entry.transcript.update(message);
        if let Some(chat_intent::Intent::OpenLink(link)) = event {
            self.open_link(ctx, &link);
        }
        task.map(move |message| Message::SubagentChat(parent.clone(), child.clone(), message))
    }

    /// Opens a read-only copy of a subagent's transcript in its own window: its library session
    /// when it has one, else its native session file read into memory. Adds no library session,
    /// runtime session or Recents entry.
    pub(super) fn detach_child_transcript(&mut self, ctx: &mut Ctx, reference: String) {
        let listings = ctx.runtime.library().store().list();
        let dom = if listings.is_ok_and(|listings| listings.iter().any(|listing| matches!(listing, Listing::Session(info) if info.id == reference))) {
            match ctx.runtime.library().load(&reference) {
                Ok(dom) => dom,
                Err(error) => return ctx.notify(format!("Can't open the subagent's transcript: {error}")),
            }
        } else {
            let path = PathBuf::from(&reference);
            if !path.is_file() {
                return ctx.notify(format!("The subagent's session {reference} isn't on this machine"));
            }
            let Some(parent) = ctx.runtime.sessions().active() else { return };
            let Some(adapter) = parent.harness_id().and_then(|harness| ctx.runtime.harnesses().registry.get(harness)) else {
                return ctx.notify("The parent session's harness isn't supported here");
            };
            let sink = MemorySink::new();
            if let Err(error) = adapter.import(&path, &sink) {
                return ctx.notify(format!("Can't open the subagent's transcript: {error}"));
            }
            sink.dom()
        };
        let path_str = reference;
        let title = self
            .agents
            .jobs()
            .find(|job| job.session() == Some(path_str.as_str()))
            .map(|job| job.label().to_owned())
            .or_else(|| SessionSummary::from_dom(&dom, false).map(|summary| summary.title))
            .unwrap_or_else(|| "Subagent transcript".to_owned());
        ctx.request(Request::DetachTranscript { key: path_str, title, dom });
    }

    /// Refreshes an open child subtab when its `<job>` transitions to a terminal status.
    pub(super) fn on_job_patch(&mut self, ctx: &Ctx, session_id: &str, node_id: NodeId) -> Task<Message> {
        let Some(node) = ctx.runtime.sessions().get(session_id).and_then(|session| session.dom().get(node_id)) else {
            return Task::none();
        };
        if node.kind != ElementKind::Job || node.str_attr(attr::KIND).and_then(JobKind::parse) != Some(JobKind::Subagent) {
            return Task::none();
        }
        let Some(child_id) = node.str_attr(attr::ID).filter(|id| !id.is_empty()) else {
            return Task::none();
        };
        let new_status = node.str_attr(attr::STATUS).and_then(JobStatus::parse).unwrap_or(JobStatus::Running);
        let new_title = node.str_attr(attr::TITLE).or_else(|| node.str_attr(attr::AGENT)).filter(|t| !t.is_empty()).map(str::to_owned);
        let Some(child) = self.subagents.parents.get_mut(session_id).and_then(|tabs| tabs.children.get_mut(child_id)) else {
            return Task::none();
        };
        if let Some(title) = new_title {
            child.tab_label = format!("{title} · {}", child.id);
            child.title = title;
        }
        let was_terminal = matches!(child.status, Some(JobStatus::Done | JobStatus::Error | JobStatus::Cancelled));
        let is_terminal = matches!(new_status, JobStatus::Done | JobStatus::Error | JobStatus::Cancelled);
        child.status = Some(new_status);
        if !was_terminal && is_terminal {
            return self.fetch_child_transcript(ctx, session_id, child_id);
        }
        Task::none()
    }

    fn can_message_child(&self, ctx: &Ctx, parent_id: &str, child_id: &str) -> bool {
        let Some(session) = ctx.runtime.sessions().get(parent_id) else { return false };
        let Some(harness) = session.harness() else { return false };
        if !capabilities(ctx, harness.harness()).has(Feature::SubagentMessages) {
            return false;
        }
        self.subagents
            .parents
            .get(parent_id)
            .and_then(|tabs| tabs.children.get(child_id))
            .is_some_and(|child| child.addressable && !child.sending)
    }

    pub(crate) fn view_child_center<'a>(&'a self, ctx: &'a Ctx) -> Option<(Element<'a, Message>, Option<Element<'a, Message>>)> {
        let parent = ctx.runtime.sessions().active()?;
        let parent_id = parent.id().as_str();
        let parent_tabs = self.subagents.parents.get(parent_id).filter(|tabs| !tabs.order.is_empty())?;
        let parent_title = parent.summary().map(|summary| summary.title.as_str()).unwrap_or("Conversation");

        let mut tabs = vec![Tab {
            label: parent_title,
            icon: Some(Icon::Bot),
            active: parent_tabs.selected.is_none(),
            on: Message::SubagentSelect(None),
            id: None,
            trailing: None,
        }];
        for child_id in &parent_tabs.order {
            if let Some(child) = parent_tabs.children.get(child_id) {
                let close = button(tinted(Icon::X, T::Caption, Tone::Muted))
                    .on_press(Message::SubagentClose(child_id.clone()))
                    .style(style::button(ButtonKind::Ghost))
                    .padding(sp(Sp::Xs) / 2.0);
                tabs.push(Tab {
                    label: child.tab_label.as_str(),
                    icon: Some(Icon::Cpu),
                    active: parent_tabs.selected.as_deref() == Some(child_id.as_str()),
                    on: Message::SubagentSelect(Some(child_id.clone())),
                    id: None,
                    trailing: Some(close.into()),
                });
            }
        }
        let thin = sp(Sp::Xs) / 2.0;
        let strip = scrollable(tab_strip(tabs))
            .direction(scrollable::Direction::Horizontal(scrollable::Scrollbar::new().width(thin).scroller_width(thin)))
            .style(style::scrollbar(true))
            .width(Fill);
        let strip: Element<'a, Message> = container(strip).style(style::container(Surface::Side)).padding([sp(Sp::Xs), sp(Sp::S)]).width(Fill).into();
        let body = parent_tabs.selected.as_deref().and_then(|id| parent_tabs.children.get(id)).map(|child| self.view_child_subtab(ctx, parent, child));
        Some((strip, body))
    }

    fn view_child_subtab<'a>(&'a self, ctx: &'a Ctx, parent: &'a Session, child: &'a ChildTab) -> Element<'a, Message> {
        let parent_id = parent.id().as_str();
        let has_harness = parent.harness().is_some();
        let harness_id = parent.harness_id().unwrap_or_default();
        let supports_messages = parent.harness().is_some_and(|harness| capabilities(ctx, harness.harness()).has(Feature::SubagentMessages));
        let can_message = has_harness && supports_messages && child.addressable && !child.sending;

        let status_badge = child.status.map(|status| {
            let tone = match status {
                JobStatus::Running => Tone::Muted,
                JobStatus::Done => Tone::Ok,
                JobStatus::Error => Tone::Danger,
                JobStatus::Cancelled => Tone::Muted,
            };
            badge(status.as_str(), tone)
        });
        let syncing = (has_harness && supports_messages && child.loading).then(|| label("Syncing…", T::Caption, Tone::Faint));
        let header = row![
            label(child.title.as_str(), T::Body, Tone::Normal),
            status_badge,
            space().width(Fill),
            syncing,
        ]
        .spacing(sp(Sp::S))
        .align_y(Alignment::Center);
        let header = container(header).padding([sp(Sp::Xs), sp(Sp::M)]).width(Fill);

        let explanatory: Option<Element<'a, Message>> = if !has_harness {
            Some(
                container(card(
                    column![
                        label("Parent session is not running", T::Label, Tone::Normal),
                        label(
                            "This conversation isn't running, so its subagents can't be reached. Resume it to message live subagents.",
                            T::Caption,
                            Tone::Muted,
                        ),
                    ]
                    .spacing(sp(Sp::Xs)),
                ))
                .padding([sp(Sp::S), sp(Sp::M)])
                .width(Fill)
                .into(),
            )
        } else if !supports_messages {
            let harness_label = ctx.runtime.harnesses().label(harness_id);
            Some(
                container(card(
                    column![
                        label("Subagent messaging not supported", T::Label, Tone::Normal),
                        label(
                            format!("{harness_label} doesn't let Loa message its subagents. Use Transcript in the Agents tab to read a copy."),
                            T::Caption,
                            Tone::Muted,
                        ),
                    ]
                    .spacing(sp(Sp::Xs)),
                ))
                .padding([sp(Sp::S), sp(Sp::M)])
                .width(Fill)
                .into(),
            )
        } else {
            None
        };

        let fetch_err = child.fetch_error.as_deref().map(|err| {
            container(label(err, T::Caption, Tone::Danger))
                .padding([sp(Sp::Xs), sp(Sp::S)])
                .style(style::container(Surface::ErrorBox))
                .width(Fill)
        });

        let transcript_area: Element<'a, Message> = match &child.dom {
            Some(dom) if dom.root().is_some_and(|root| dom.children(root.id).next().is_some()) => {
                let (p, c) = (parent_id.to_owned(), child.id.clone());
                child.transcript.view(dom, move |message| Message::SubagentChat(p.clone(), c.clone(), message), |_| space().into())
            }
            Some(_) if has_harness && supports_messages => {
                container(label("No transcript messages yet", T::Label, Tone::Muted)).center(Fill).into()
            }
            None if has_harness && supports_messages => {
                let text = if child.loading { "Loading subagent transcript…" } else { "No transcript loaded" };
                container(label(text, T::Label, Tone::Muted)).center(Fill).into()
            }
            _ => space().height(Fill).into(),
        };

        let send_err = child.send_error.as_deref().map(|err| {
            container(label(err, T::Caption, Tone::Danger))
                .padding([sp(Sp::Xs), sp(Sp::S)])
                .style(style::container(Surface::ErrorBox))
                .width(Fill)
        });
        let read_only_note = (has_harness && supports_messages && !child.addressable && child.dom.is_some() && !child.loading).then(|| {
            let note = match child.status {
                Some(JobStatus::Done | JobStatus::Error | JobStatus::Cancelled) => format!("{} has finished, so OMP no longer accepts messages for it. Its conversation stays readable here.", child.id),
                _ => format!("{} isn't accepting messages right now. Its conversation stays readable here.", child.id),
            };
            label(note, T::Caption, Tone::Muted)
        });

        let placeholder = if !has_harness {
            "Parent session is not running"
        } else if !supports_messages {
            "Messaging subagents is not supported"
        } else if child.sending {
            "Sending…"
        } else if !child.addressable {
            "Subagent is not accepting messages"
        } else {
            "Message subagent…"
        };
        let mut input = text_input(placeholder, &child.draft)
            .size(ts(T::Body))
            .style(style::input)
            .padding([sp(Sp::Xs), sp(Sp::S)])
            .width(Fill);
        if can_message {
            let (p, c) = (parent_id.to_owned(), child.id.clone());
            input = input.on_input(move |text| Message::SubagentDraft(p.clone(), c.clone(), text));
            if !child.draft.trim().is_empty() {
                input = input.on_submit(Message::SubagentSend(parent_id.to_owned(), child.id.clone()));
            }
        }
        let send = text_button(
            if child.sending { "Sending…" } else { "Send" },
            ButtonKind::Primary,
            (can_message && !child.draft.trim().is_empty()).then(|| Message::SubagentSend(parent_id.to_owned(), child.id.clone())),
        );
        let composer = container(
            column![
                send_err,
                read_only_note,
                row![input, send].spacing(sp(Sp::S)).align_y(Alignment::Center),
            ]
            .spacing(sp(Sp::Xs)),
        )
        .padding([sp(Sp::S), sp(Sp::M)])
        .width(Fill);

        column![
            header,
            rule::horizontal(1).style(style::rule),
            explanatory,
            fetch_err,
            container(transcript_area).width(Fill).height(Fill),
            rule::horizontal(1).style(style::rule),
            (has_harness && supports_messages).then_some(composer),
        ]
        .height(Fill)
        .into()
    }
}
