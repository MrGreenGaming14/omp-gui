//! The remote hosts section: questions waiting for the user, the add form and the host list
//! with its limits editor and audit log.

use super::{BOLD, card, clipped, flat, form_buttons, form_card, inset, note_row};
use crate::tools::{AuditRow, FormId, Limit, Message, PromptAnswer, PromptKind, RemoteHost, RemotePrompt, State};
use iced::widget::{Column, checkbox, column, container, row, rule, scrollable, space, text};
use iced::{Alignment, Element, Fill, Font, Padding, Shrink};
use loa_ui_kit::icons::{Icon, tinted};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole as Sp, TextRole as T, pad_y, sp, text_size as ts};
use loa_ui_kit::widgets::{form_view, label, list_editor};

/// Height of the audit log's own scroll area.
const AUDIT_HEIGHT: f32 = 260.0;

impl State {
    pub(super) fn visible_hosts(&self) -> Vec<&RemoteHost> { self.hosts.iter().filter(|h| self.filter.matches(&h.alias) || self.filter.matches(&h.hostname)).collect() }

    pub(super) fn hosts_view(&self, wide: bool) -> Element<'_, Message> {
        let prompts = (!self.prompts.is_empty()).then(|| {
            let head = row![label("Waiting for you", T::Caption, Tone::Warn), label(self.prompts.len().to_string(), T::Caption, Tone::Faint)].spacing(sp(Sp::Xs));
            Column::new().push(head).extend(self.prompts.iter().map(prompt_card)).spacing(sp(Sp::Xs))
        });
        let add = self.add_host.as_ref().map(|f| form_card("Add host", None, form_view(&f.spec, &f.errors, |k, v| Message::FormEdit(FormId::Host, k, v)), "Add", FormId::Host));
        let hosts = self.visible_hosts();
        let actions = row![flat("Import from ssh config", None).on_press(Message::ImportSshHosts), flat("Add host", Some(Icon::Plus)).on_press(Message::FormOpen(FormId::Host))].spacing(sp(Sp::Xs) / 2.0);
        let header = self.block_header("hosts", "Hosts".to_owned(), hosts.len(), Some(actions.into()));
        let list = self.block_open("hosts").then(|| if hosts.is_empty() { card([note_row("No remote hosts")]) } else { card(hosts.into_iter().map(|h| self.host_view(h, wide))) });
        column![prompts, add, header, list].spacing(sp(Sp::S)).into()
    }

    /// Alias and address, the limits in a line and the actions; the limits editor and the
    /// audit log open under it.
    fn host_view<'a>(&'a self, host: &'a RemoteHost, wide: bool) -> Element<'a, Message> {
        let alias = host.alias.as_str();
        let target = format!("{}{}{}{}", host.user.as_ref().map_or(String::new(), |u| format!("{u}@")), host.hostname, host.port.map_or(String::new(), |p| format!(":{p}")), host.proxy_jump.as_ref().map_or(String::new(), |j| format!(" via {j}")));
        let l = &host.limits;
        let paths = if l.allowed_paths.is_empty() { "no paths".to_owned() } else { l.allowed_paths.join(", ") };
        let flags = [(l.read_only, "read only", "read write"), (l.commands_allowed, "commands allowed", "no commands"), (l.auto_approve, "runs without asking", "asks first")].map(|(on, yes, no)| if on { yes } else { no });
        let summary = clipped(label(format!("{} · {paths}", flags.join(" · ")), T::Caption, Tone::Faint).wrapping(text::Wrapping::None));
        let act = |s, m| flat(s, None).on_press(m);
        let actions = row![act("Edit limits", Message::EditLimits(alias.into())), act("Terminal", Message::OpenTerminal(alias.into())), act("Audit log", Message::ShowAudit(alias.into())), act("Remove", Message::RemoveHost(alias.into())).style(style::button(ButtonKind::Danger))].spacing(sp(Sp::Xs) / 2.0);
        let name = row![text(alias).size(ts(T::Label)).font(BOLD).wrapping(text::Wrapping::None), clipped(text(target).size(ts(T::Caption)).style(style::text(Tone::Muted)).font(Font::MONOSPACE).wrapping(text::Wrapping::None))].spacing(sp(Sp::S)).align_y(Alignment::Center);
        let head: Element<'a, Message> = if wide { column![row![name, actions].spacing(sp(Sp::S)).align_y(Alignment::Center), summary].into() } else { column![name, summary, actions].into() };
        let editor = self.limits.as_ref().filter(|d| d.host == host.alias).map(|d| {
            let check = |on, s, limit| checkbox(on).label(s).size(ts(T::Body)).text_size(ts(T::Caption)).spacing(sp(Sp::S)).style(style::check).on_toggle(move |v| Message::LimitSet(limit, v));
            let paths = list_editor(&d.limits.allowed_paths, Clone::clone, Message::LimitsRemovePath, &d.new_path, |v| Message::FormEdit(FormId::Limits, "path", v), Message::LimitsAddPath);
            let flags = row![check(d.limits.read_only, "Read only", Limit::ReadOnly), check(d.limits.commands_allowed, "Commands allowed", Limit::Commands)].spacing(sp(Sp::L));
            let error = d.path_error.map(|e| label(e, T::Caption, Tone::Danger));
            inset(column![flags, check(d.limits.auto_approve, "Run writes and commands without asking", Limit::AutoApprove), label("Allowed paths", T::Caption, Tone::Muted), paths, error, form_buttons("Save", FormId::Limits)].spacing(sp(Sp::S)))
        });
        let audit = self.audit.as_ref().filter(|(h, _)| h == alias).map(|(_, rows)| inset(audit_log(rows)));
        column![container(head).padding(pad_y(Sp::Xs).right(sp(Sp::Xs)).left(sp(Sp::M))), editor, audit].into()
    }
}

/// A host's audit log, newest first, in a scroll area of its own.
fn audit_log(rows: &[AuditRow]) -> Element<'_, Message> {
    let header = row![label("Audit log", T::Caption, Tone::Muted), label(rows.len().to_string(), T::Caption, Tone::Faint), space().width(Fill), flat("Close", Some(Icon::X)).on_press(Message::CloseAudit)].spacing(sp(Sp::Xs)).align_y(Alignment::Center);
    if rows.is_empty() { return column![header, label("No entries", T::Caption, Tone::Faint)].spacing(sp(Sp::Xs)).into() }
    let entries = rows.iter().enumerate().map(|(i, e)| {
        let ok = e.outcome == "ok" || e.outcome.starts_with("ok:");
        let what = row![label(e.action.label(), T::Caption, Tone::Normal), clipped(text(e.detail.as_str()).size(ts(T::Caption)).font(Font::MONOSPACE).wrapping(text::Wrapping::None)), label(e.when.as_str(), T::Caption, Tone::Faint)].spacing(sp(Sp::S));
        let how = row![clipped(label(e.outcome.as_str(), T::Caption, if ok { Tone::Ok } else { Tone::Danger }).wrapping(text::Wrapping::None)), label(e.session.as_str(), T::Caption, Tone::Faint)].spacing(sp(Sp::S));
        column![(i > 0).then(|| rule::horizontal(1).style(style::rule)), what, how].spacing(sp(Sp::Xs) / 2.0).into()
    });
    let log = scrollable(container(Column::with_children(entries).spacing(sp(Sp::Xs))).padding(Padding::ZERO.right(sp(Sp::S)))).style(style::scrollbar(false)).height(Shrink);
    column![header, container(log).max_height(AUDIT_HEIGHT)].spacing(sp(Sp::Xs)).into()
}

/// A host key or approval question with its answers.
fn prompt_card(p: &RemotePrompt) -> Element<'_, Message> {
    let answer = |s, a| flat(s, None).on_press(Message::AnswerPrompt(p.id, a));
    let danger = style::button(ButtonKind::Danger);
    let answers = match p.kind {
        PromptKind::HostKey => row![answer("Trust", PromptAnswer::Trust), answer("Reject", PromptAnswer::Reject).style(danger)],
        PromptKind::Approval => row![answer("Allow once", PromptAnswer::AllowOnce), answer("Allow for session", PromptAnswer::AllowSession), answer("Deny", PromptAnswer::Deny).style(danger)],
    };
    let title = row![tinted(Icon::TriangleAlert, T::Caption, Tone::Warn), text(p.title.as_str()).size(ts(T::Label))].spacing(sp(Sp::S)).align_y(Alignment::Center);
    let body = text(p.body.as_str()).size(ts(T::Caption)).font(Font::MONOSPACE).style(style::text(Tone::Muted));
    container(column![title, body, answers.spacing(sp(Sp::Xs) / 2.0)].spacing(sp(Sp::Xs))).padding(sp(Sp::M)).width(Fill).style(style::container(Surface::Attention)).into()
}
