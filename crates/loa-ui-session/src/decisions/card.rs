//! One decision card: its input state and view, with per-kind behaviour in [`CardKind`]
//! strategies (approval, question, plan review, form) picked by [`kind_for`].

use super::form::{FieldType, FieldValue, Form, FormError};
use super::model::{ApprovalChoice, Choice, Decision, PlanChoice, answer_summary};
use super::{Message, TIMEOUT_BY, node_name};
use iced::alignment::Vertical;
use iced::widget::{button, checkbox, column, container, markdown, pick_list, row, text, text_editor, text_input};
use iced::{Element, Fill, Font};
use loa_core::{DecisionKind, DecisionStatus, NodeId};
use loa_ui_kit::icons::icon;
use loa_ui_kit::style::{self as kit, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{ColorRole as C, SpaceRole as Sp, TextRole as T, sp, text_size as ts};
use loa_ui_panel::diff::{FileDiff, Tag, parse_unified};
use loa_view::theme::Icon;

/// Values and reason of an answer, before the answering user is added.
#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub values: Vec<String>,
    pub reason: Option<String>,
}

impl Reply {
    fn one(value: impl Into<String>, reason: Option<String>) -> Option<Self> { Some(Self { values: vec![value.into()], reason }) }
}

/// Id of a card's deny reason, reject feedback or typed answer field.
pub fn text_field_id(decision: NodeId) -> iced::widget::Id { iced::widget::Id::from(format!("decision-{decision}-text")) }
/// Id of the text input of form field `index`.
pub fn form_field_id(decision: NodeId, index: usize) -> iced::widget::Id { iced::widget::Id::from(format!("decision-{decision}-field-{index}")) }

/// What the user typed or picked into a form field; `Toggle(n)` flips option `n` of a multi field.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldInput {
    Text(String),
    Bool(bool),
    Choice(usize),
    Toggle(usize),
}

pub struct Card {
    pub decision: Decision,
    /// Answered locally, waiting for the DOM to record it.
    pub pending: bool,
    /// Option highlighted by a number key, confirmed by Enter.
    pub selected: Option<usize>,
    /// Multi choice questions: which options are ticked.
    pub picked: Vec<bool>,
    /// Deny reason, reject feedback or the typed answer.
    pub text: String,
    plan: Vec<markdown::Item>,
    pub editor: Option<text_editor::Content>,
    pub form: Option<Result<Form, FormError>>,
    /// The approval body parsed as a unified diff, with its line count; parsed when the body
    /// changes, not per frame.
    diff: Option<(Vec<FileDiff>, usize)>,
    /// Whether an approval diff longer than the preview is shown in full.
    pub show_full: bool,
}

/// Lines of a diff shown before the rest waits behind a button.
pub const PREVIEW_LINES: usize = 300;

/// How a card is drawn.
pub struct CardCtx<'a> {
    pub focused: bool,
    pub view_only: bool,
    pub markdown: markdown::Settings,
    /// The viewer's key and the local node's name, for "answered by".
    pub names: (&'a str, Option<&'a str>),
}

impl Card {
    pub fn new(decision: Decision) -> Self {
        let (plan, form, diff) = (Vec::new(), None, None);
        let mut card = Self { picked: Vec::new(), plan, form, diff, show_full: false, decision, pending: false, selected: None, text: String::new(), editor: None };
        card.parse(true, true, true);
        card
    }

    fn parse(&mut self, body: bool, fields: bool, options: bool) {
        let d = &self.decision;
        if body {
            self.plan = if d.kind == DecisionKind::Plan { markdown::parse(&d.body).collect() } else { Vec::new() };
            self.diff = (d.kind == DecisionKind::Approval).then(|| parse_diff(&d.body)).flatten().map(|files| (files, d.body.lines().count()));
        }
        if fields {
            self.form = d.fields.as_deref().filter(|_| d.kind == DecisionKind::Form).map(Form::parse);
        }
        if options {
            (self.picked, self.selected) = (vec![false; d.options.len()], None);
        }
        if !d.is_waiting() {
            (self.pending, self.editor) = (false, None);
        }
    }

    /// Takes the latest DOM state, keeping what the user entered while it waits.
    pub fn refresh(&mut self, decision: Decision) {
        if decision == self.decision {
            return;
        }
        let old = std::mem::replace(&mut self.decision, decision);
        let d = &self.decision;
        self.parse(old.body != d.body, old.fields != d.fields, old.options != d.options);
    }

    /// Can take input from the local user.
    pub fn open(&self) -> bool { self.decision.is_waiting() && !self.pending }
    pub fn kind(&self) -> &'static dyn CardKind { kind_for(self.decision.kind) }

    /// Number key: highlight option `index` (tick it on a multi choice question).
    pub fn pick(&mut self, index: usize) {
        if index < self.kind().options(self) {
            if self.decision.multi && self.decision.kind == DecisionKind::Question {
                self.picked[index] = !self.picked[index];
            }
            self.selected = Some(index);
        }
    }

    pub fn choose(&mut self, index: usize) -> Option<Reply> { if index < self.kind().options(self) { self.kind().choose(self, index) } else { None } }

    pub fn set_field(&mut self, index: usize, input: FieldInput) {
        if let Some(Ok(form)) = &mut self.form {
            match input {
                FieldInput::Text(t) => form.set(index, FieldValue::Text(t)),
                FieldInput::Bool(b) => form.set(index, FieldValue::Bool(b)),
                FieldInput::Choice(i) => form.set(index, FieldValue::Choice(Some(i))),
                FieldInput::Toggle(option) => form.toggle(index, option),
            }
        }
    }

    fn reason(&self) -> Option<String> { Some(self.text.trim()).filter(|t| !t.is_empty()).map(str::to_owned) }

    pub fn view<'a>(&'a self, cx: &CardCtx) -> Element<'a, Message> {
        let d = &self.decision;
        let (glyph, label) = self.kind().header(d);
        let who = d.harness.as_deref().unwrap_or("The agent");
        let accent = |s: String| text(s).size(ts(T::Body)).style(kit::text(Tone::Accent));
        let mut head = row![icon(glyph, ts(T::Body)), accent(format!("{who} {label}"))].push(d.tool.clone().map(accent));
        // An approval naming its tool says nothing more; other headers show the title.
        if !(d.kind == DecisionKind::Approval && d.tool.is_some()) {
            head = head.push(d.title.as_deref().map(|t| text(t).size(ts(T::Label)).style(kit::text(Tone::Muted))));
        }
        let active = self.open() && !cx.view_only;
        let body = column![head.spacing(sp(Sp::S)).align_y(Vertical::Center), self.kind().body(self, active, cx)].push(self.footer(cx)).spacing(sp(Sp::M));
        container(body).padding(sp(Sp::M)).width(Fill).style(kit::container(if cx.focused { Surface::Attention } else { Surface::Card })).into()
    }

    /// Status line: who answered, why it closed, or why it can't be clicked.
    fn footer<'a>(&'a self, cx: &CardCtx) -> Option<Element<'a, Message>> {
        let d = &self.decision;
        let (local, name) = cx.names;
        let by = |prefix: &str| match d.by.as_deref() {
            Some(TIMEOUT_BY) => format!("{prefix} by timeout (default answer)"),
            Some(by) => format!("{prefix} by {}", node_name(by, local, name)),
            None => prefix.to_owned(),
        };
        let (status, tone) = match d.status {
            DecisionStatus::Waiting if self.pending => ("Sending answer…".to_owned(), Tone::Muted),
            DecisionStatus::Waiting if cx.view_only => ("View only: you can't answer this prompt".to_owned(), Tone::Muted),
            DecisionStatus::Waiting => return None,
            DecisionStatus::Answered => (by(&answer_summary(d.kind, &d.answer)), d.answer.first().map_or(Tone::Muted, |a| self.kind().outcome(a))),
            DecisionStatus::Expired => (by("Expired"), Tone::Muted),
            DecisionStatus::Cancelled => ("Cancelled".to_owned(), Tone::Muted),
        };
        let mut footer = column![text(status).size(ts(T::Body)).style(kit::text(tone))].spacing(sp(Sp::S));
        if d.status == DecisionStatus::Answered {
            if let (DecisionKind::Form, [json]) = (d.kind, d.answer.as_slice()) {
                footer = footer.push(text(json).size(ts(T::Label)).font(Font::MONOSPACE));
            }
            let edited = d.kind == DecisionKind::Plan && d.answer.first().and_then(|a| PlanChoice::from_answer(a)) == Some(PlanChoice::Edit);
            footer = footer.push(d.reason.as_deref().map(|r| text(r).size(ts(T::Label)).style(kit::text(Tone::Muted)).font(if edited { Font::MONOSPACE } else { Font::DEFAULT })));
        }
        Some(footer.into())
    }
}

/// Per-kind card behaviour.
pub trait CardKind: Sync {
    /// Header icon and what the agent wants, after its name: "wants to run".
    fn header(&self, d: &Decision) -> (Icon, &'static str);
    /// The system notification's tail: "needs approval".
    fn notice(&self) -> &'static str;
    /// The unattended settings row: "Approvals".
    fn settings_label(&self) -> &'static str;
    /// Options number keys address.
    fn options(&self, c: &Card) -> usize;
    /// A click on option `index < options`.
    fn choose(&self, c: &mut Card, index: usize) -> Option<Reply>;
    /// Enter, or the submit button.
    fn confirm(&self, c: &mut Card) -> Option<Reply>;
    /// Esc: deny, reject (or leave plan editing), skip.
    fn dismiss(&self, c: &mut Card) -> Option<Reply>;
    /// Enter in the card's text field.
    fn submit_text(&self, c: &mut Card) -> Option<Reply>;
    /// What sits between header and footer.
    fn body<'a>(&self, c: &'a Card, active: bool, cx: &CardCtx) -> Element<'a, Message>;
    /// Whether `d.default` is a valid answer.
    fn valid_default(&self, d: &Decision) -> bool;
    /// Tone of an answered card's status line, by its first value.
    fn outcome(&self, _first: &str) -> Tone { Tone::Normal }
    /// The answer the unattended policy's `configured` value gives `d`, and whether it refuses.
    fn policy(&self, d: &Decision, configured: &str) -> Option<(String, bool)>;
    /// Answers the unattended settings offer, as `(answer, label)`; empty takes typed text.
    fn presets(&self) -> Vec<(&'static str, &'static str)>;
}

/// The strategy of `kind`.
pub fn kind_for(kind: DecisionKind) -> &'static dyn CardKind {
    match kind {
        DecisionKind::Approval => &Approval,
        DecisionKind::Question => &Question,
        DecisionKind::Plan => &Plan,
        DecisionKind::Form => &FormCard,
    }
}

fn text_field<'a>(c: &'a Card, placeholder: &'a str, active: bool) -> Element<'a, Message> {
    let id = c.decision.id;
    let input = text_input(placeholder, &c.text).id(text_field_id(id)).size(ts(T::Label)).padding([sp(Sp::S), sp(Sp::M)]).style(kit::input);
    input.on_input_maybe(active.then_some(move |t| Message::Text(id, t))).on_submit_maybe(active.then_some(Message::SubmitText(id))).into()
}

/// A labeled card button; `primary` is the answer the card leans toward, `selected` the option
/// a number key highlighted.
fn action<'a>(label: impl text::IntoFragment<'a>, on: Option<Message>, primary: bool, selected: bool) -> button::Button<'a, Message> {
    let kind = if primary { ButtonKind::Accent } else if selected { ButtonKind::Tab { active: true } } else { ButtonKind::Secondary };
    button(text(label).size(ts(T::Label))).padding([sp(Sp::Xs), sp(Sp::M)]).style(kit::button(kind)).on_press_maybe(on)
}

/// A numbered option button.
fn option<'a>(c: &Card, i: usize, label: &'a str, active: bool, primary: bool) -> Element<'a, Message> {
    let content = row![text(i + 1).font(Font::MONOSPACE).size(ts(T::Caption)), text(label).size(ts(T::Label))].spacing(sp(Sp::S)).align_y(Vertical::Center);
    let kind = if primary { ButtonKind::Accent } else if c.selected == Some(i) { ButtonKind::Tab { active: true } } else { ButtonKind::Secondary };
    button(content).padding([sp(Sp::Xs), sp(Sp::M)]).style(kit::button(kind)).on_press_maybe(active.then_some(Message::Choose(c.decision.id, i))).into()
}

fn choices<'a, K: Choice>(c: &Card, active: bool, primary: impl Fn(K) -> bool) -> Element<'a, Message> {
    row(K::ALL.iter().enumerate().map(|(i, k)| option(c, i, k.label(), active, primary(*k)))).spacing(sp(Sp::S)).into()
}

fn on(c: &Card, active: bool, message: fn(NodeId) -> Message) -> Option<Message> { active.then(|| message(c.decision.id)) }

struct Approval;
impl Approval {
    fn reply(c: &Card, choice: ApprovalChoice) -> Option<Reply> { Reply::one(choice.answer(), if choice == ApprovalChoice::Deny { c.reason() } else { None }) }
}
impl CardKind for Approval {
    fn header(&self, d: &Decision) -> (Icon, &'static str) { (Icon::TriangleAlert, if d.tool.is_some() { "wants to run" } else { "needs approval" }) }
    fn notice(&self) -> &'static str { "needs approval" }
    fn settings_label(&self) -> &'static str { "Approvals" }
    fn options(&self, _: &Card) -> usize { ApprovalChoice::ALL.len() }
    fn choose(&self, c: &mut Card, i: usize) -> Option<Reply> { Self::reply(c, ApprovalChoice::ALL[i]) }
    fn confirm(&self, c: &mut Card) -> Option<Reply> { Self::reply(c, ApprovalChoice::ALL[c.selected?]) }
    fn dismiss(&self, c: &mut Card) -> Option<Reply> { Self::reply(c, ApprovalChoice::Deny) }
    fn submit_text(&self, c: &mut Card) -> Option<Reply> { Self::reply(c, ApprovalChoice::Deny) }
    fn body<'a>(&self, c: &'a Card, active: bool, _: &CardCtx) -> Element<'a, Message> {
        let body = c.decision.body.as_str();
        let preview: Element<'a, Message> = match &c.diff {
            Some((files, lines)) => {
                let toggle = (*lines > PREVIEW_LINES).then(|| {
                    let label = if c.show_full { "Show fewer lines".to_owned() } else { format!("Show all {lines} lines") };
                    button(text(label).size(ts(T::Label))).style(kit::button(ButtonKind::Ghost)).on_press(Message::ToggleDiff(c.decision.id))
                });
                column![diff_view(files, c.show_full)].push(toggle).spacing(sp(Sp::S)).into()
            }
            None => container(text(body).size(ts(T::Label)).font(Font::MONOSPACE)).padding([sp(Sp::Xs), sp(Sp::S)]).width(Fill).style(kit::container(Surface::Raised)).into(),
        };
        let waiting = c.decision.is_waiting().then(|| column![choices(c, active, |k: ApprovalChoice| k != ApprovalChoice::Deny), text_field(c, "Reason for denying (optional)", active)].spacing(sp(Sp::M)));
        column![preview].push(waiting).spacing(sp(Sp::M)).into()
    }
    fn valid_default(&self, d: &Decision) -> bool { matches!(d.default.as_slice(), [a] if ApprovalChoice::from_answer(a).is_some()) }
    fn outcome(&self, first: &str) -> Tone {
        match ApprovalChoice::from_answer(first) {
            Some(ApprovalChoice::Deny) => Tone::Danger,
            Some(_) => Tone::Ok,
            None => Tone::Normal,
        }
    }
    fn policy(&self, _: &Decision, configured: &str) -> Option<(String, bool)> { ApprovalChoice::from_answer(configured).map(|c| (configured.to_owned(), c == ApprovalChoice::Deny)) }
    fn presets(&self) -> Vec<(&'static str, &'static str)> { ApprovalChoice::ALL.iter().rev().map(|c| (c.answer(), c.label())).collect() }
}

struct Question;
impl Question {
    fn typed(c: &Card) -> Option<Reply> { Reply::one(c.reason()?, None) }
}
impl CardKind for Question {
    fn header(&self, _: &Decision) -> (Icon, &'static str) { (Icon::CircleDot, "asks") }
    fn notice(&self) -> &'static str { "has a question" }
    fn settings_label(&self) -> &'static str { "Questions" }
    fn options(&self, c: &Card) -> usize { c.decision.options.len() }
    fn choose(&self, c: &mut Card, i: usize) -> Option<Reply> {
        if !c.decision.multi {
            return Reply::one(c.decision.options[i].clone(), None);
        }
        c.picked[i] = !c.picked[i];
        c.selected = Some(i);
        None
    }
    fn confirm(&self, c: &mut Card) -> Option<Reply> {
        let d = &c.decision;
        match c.selected {
            _ if d.multi && c.picked.contains(&true) => Some(Reply { values: d.options.iter().zip(&c.picked).filter(|(_, p)| **p).map(|(o, _)| o.clone()).collect(), reason: None }),
            Some(i) if !d.multi => Reply::one(d.options[i].clone(), None),
            _ => Self::typed(c),
        }
    }
    fn dismiss(&self, _: &mut Card) -> Option<Reply> { Some(Reply { values: Vec::new(), reason: None }) }
    fn submit_text(&self, c: &mut Card) -> Option<Reply> { Self::typed(c) }
    fn body<'a>(&self, c: &'a Card, active: bool, _: &CardCtx) -> Element<'a, Message> {
        let d = &c.decision;
        let mut body = column![text(&d.body).size(ts(T::Body))].spacing(sp(Sp::M));
        if d.is_waiting() {
            let options = d.options.iter().enumerate().map(|(i, o)| match d.multi {
                true => checkbox(c.picked[i]).label(format!("{} {o}", i + 1)).text_size(ts(T::Label)).style(kit::check).on_toggle_maybe(active.then_some(move |_| Message::Choose(d.id, i))).into(),
                false => option(c, i, o, active, false),
            });
            let answer = row![text_field(c, "Type your own answer", active), action("Send", on(c, active, Message::SubmitText), true, false)];
            let answer = answer.push(d.multi.then(|| action("Submit", on(c, active, Message::Submit), true, false))).push(action("Skip", on(c, active, Message::Skip), false, false));
            body = body.push(column(options).spacing(sp(Sp::S))).push(answer.align_y(Vertical::Center).spacing(sp(Sp::S)));
        }
        body.into()
    }
    fn valid_default(&self, d: &Decision) -> bool {
        match d.default.as_slice() {
            [] => false,
            [_] => true,
            many => d.multi && many.iter().all(|a| d.options.contains(a)),
        }
    }
    /// An option label picks that option; anything else is the typed answer.
    fn policy(&self, d: &Decision, configured: &str) -> Option<(String, bool)> {
        Some((d.options.iter().find(|o| o.eq_ignore_ascii_case(configured)).map_or(configured, String::as_str).to_owned(), false))
    }
    fn presets(&self) -> Vec<(&'static str, &'static str)> { Vec::new() }
}

struct Plan;
impl Plan {
    fn reply(c: &mut Card, choice: PlanChoice) -> Option<Reply> {
        match choice {
            PlanChoice::Approve => Reply::one(choice.answer(), None),
            PlanChoice::Edit => {
                (c.editor, c.selected) = (Some(text_editor::Content::with_text(&c.decision.body)), None);
                None
            }
            PlanChoice::Reject => Reply::one(choice.answer(), c.reason()),
        }
    }
}
impl CardKind for Plan {
    fn header(&self, _: &Decision) -> (Icon, &'static str) { (Icon::FileText, "proposes a plan") }
    fn notice(&self) -> &'static str { "has a plan to review" }
    fn settings_label(&self) -> &'static str { "Plan reviews" }
    fn options(&self, c: &Card) -> usize { if c.editor.is_none() { PlanChoice::ALL.len() } else { 0 } }
    fn choose(&self, c: &mut Card, i: usize) -> Option<Reply> { Self::reply(c, PlanChoice::ALL[i]) }
    fn confirm(&self, c: &mut Card) -> Option<Reply> {
        match &c.editor {
            Some(editor) => Reply::one(PlanChoice::Edit.answer(), Some(editor.text())),
            None => Self::reply(c, PlanChoice::ALL[c.selected?]),
        }
    }
    fn dismiss(&self, c: &mut Card) -> Option<Reply> { if c.editor.take().is_some() { None } else { Self::reply(c, PlanChoice::Reject) } }
    fn submit_text(&self, c: &mut Card) -> Option<Reply> { Self::reply(c, PlanChoice::Reject) }
    fn body<'a>(&self, c: &'a Card, active: bool, cx: &CardCtx) -> Element<'a, Message> {
        let id = c.decision.id;
        if let Some(editor) = &c.editor {
            // Without `on_action` the editor is read only.
            let editor = text_editor(editor).height(320).size(ts(T::Label)).font(Font::MONOSPACE).style(kit::editor);
            let editor = if active { editor.on_action(move |a| Message::EditPlan(id, a)) } else { editor };
            let buttons = row![action("Submit edited plan", on(c, active, Message::Submit), true, false), action("Cancel", on(c, active, Message::CancelEdit), false, false)];
            return column![editor, buttons.spacing(sp(Sp::S))].spacing(sp(Sp::M)).into();
        }
        let waiting = c.decision.is_waiting().then(|| column![choices(c, active, |k: PlanChoice| k == PlanChoice::Approve), text_field(c, "Feedback for rejecting", active)].spacing(sp(Sp::M)));
        column![markdown::view(&c.plan, cx.markdown).map(Message::Link)].push(waiting).spacing(sp(Sp::M)).into()
    }
    fn valid_default(&self, d: &Decision) -> bool { matches!(d.default.as_slice(), [a] if PlanChoice::from_answer(a).is_some_and(|c| c != PlanChoice::Edit)) }
    fn outcome(&self, first: &str) -> Tone {
        match PlanChoice::from_answer(first) {
            Some(PlanChoice::Reject) => Tone::Danger,
            Some(_) => Tone::Ok,
            None => Tone::Normal,
        }
    }
    fn policy(&self, _: &Decision, configured: &str) -> Option<(String, bool)> {
        PlanChoice::from_answer(configured).filter(|c| *c != PlanChoice::Edit).map(|c| (configured.to_owned(), c == PlanChoice::Reject))
    }
    fn presets(&self) -> Vec<(&'static str, &'static str)> { [PlanChoice::Reject, PlanChoice::Approve].map(|c| (c.answer(), c.label())).to_vec() }
}

/// Form answer that submits the defaults the form declares.
pub(crate) const FORM_DEFAULTS: &str = "{}";

struct FormCard;
impl CardKind for FormCard {
    fn header(&self, _: &Decision) -> (Icon, &'static str) { (Icon::SlidersHorizontal, "needs input") }
    fn notice(&self) -> &'static str { "needs a form filled in" }
    fn settings_label(&self) -> &'static str { "Forms" }
    fn options(&self, _: &Card) -> usize { 0 }
    fn choose(&self, _: &mut Card, _: usize) -> Option<Reply> { None }
    fn confirm(&self, c: &mut Card) -> Option<Reply> {
        match &mut c.form {
            Some(Ok(form)) => Reply::one(form.submit()?, None),
            _ => None,
        }
    }
    fn dismiss(&self, _: &mut Card) -> Option<Reply> { Some(Reply { values: Vec::new(), reason: None }) }
    fn submit_text(&self, _: &mut Card) -> Option<Reply> { None }
    fn body<'a>(&self, c: &'a Card, active: bool, _: &CardCtx) -> Element<'a, Message> {
        let d = &c.decision;
        let mut body = column![].push((!d.body.is_empty()).then(|| text(&d.body).size(ts(T::Body)))).spacing(sp(Sp::M));
        let skip = action("Skip", on(c, active, Message::Skip), false, false);
        let problem = match &c.form {
            _ if !d.is_waiting() => return body.into(),
            Some(Ok(form)) => return body.push(form_view(d.id, form, active)).push(row![action("Submit", on(c, active, Message::Submit), true, false), skip].spacing(sp(Sp::S))).into(),
            Some(Err(error)) => format!("This form can't be shown: {error}"),
            None => "This form declares no fields".to_owned(),
        };
        body = body.push(text(problem).size(ts(T::Label)).style(kit::text(Tone::Danger)));
        body.push(skip).into()
    }
    fn valid_default(&self, d: &Decision) -> bool { matches!(d.default.as_slice(), [json] if serde_json::from_str::<serde_json::Value>(json).is_ok_and(|v| v.is_object())) }
    /// The form's declared defaults with the configured object's values over them.
    fn policy(&self, d: &Decision, configured: &str) -> Option<(String, bool)> {
        let Ok(serde_json::Value::Object(overrides)) = serde_json::from_str(configured) else { return None };
        let defaults = d.fields.as_deref().and_then(|s| Form::parse(s).ok()?.submit()).and_then(|json| serde_json::from_str(&json).ok());
        let mut values: serde_json::Map<String, serde_json::Value> = defaults.unwrap_or_default();
        values.extend(overrides);
        Some((serde_json::Value::Object(values).to_string(), false))
    }
    fn presets(&self) -> Vec<(&'static str, &'static str)> { vec![(FORM_DEFAULTS, "Submit the form's defaults")] }
}

fn form_view<'a>(id: NodeId, form: &'a Form, active: bool) -> Element<'a, Message> {
    let fields = form.fields.iter().zip(&form.values).zip(&form.errors).enumerate().map(|(i, ((field, value), error))| {
        let set = move |input| Message::Field(id, i, input);
        let label = if field.required { format!("{} *", field.label) } else { field.label.clone() };
        let labels = match &field.ty {
            FieldType::Enum { labels, .. } | FieldType::Multi { labels, .. } => labels.as_slice(),
            _ => &[],
        };
        let input: Element<'a, Message> = match value {
            FieldValue::Bool(on) => checkbox(*on).label(field.label.as_str()).text_size(ts(T::Label)).style(kit::check).on_toggle_maybe(active.then_some(move |b| set(FieldInput::Bool(b)))).into(),
            FieldValue::Choice(choice) if active => {
                let pick = move |label: String| set(FieldInput::Choice(labels.iter().position(|l| *l == label).unwrap_or_default()));
                pick_list(labels, choice.and_then(|c| labels.get(c)), pick).placeholder("Choose…").text_size(ts(T::Label)).style(kit::pick).into()
            }
            FieldValue::Choice(choice) => text(choice.and_then(|c| labels.get(c)).map_or("", String::as_str)).into(),
            FieldValue::Picks(picks) => column(labels.iter().zip(picks).enumerate().map(|(n, (label, on))| {
                checkbox(*on).label(label.as_str()).text_size(ts(T::Label)).style(kit::check).on_toggle_maybe(active.then_some(move |_| set(FieldInput::Toggle(n)))).into()
            }))
            .spacing(sp(Sp::S))
            .into(),
            FieldValue::Text(raw) => {
                let input = text_input(&field.name, raw).id(form_field_id(id, i)).size(ts(T::Label)).padding([sp(Sp::S), sp(Sp::M)]).style(kit::input);
                input.on_input_maybe(active.then_some(move |t| set(FieldInput::Text(t)))).on_submit_maybe(active.then_some(Message::Submit(id))).into()
            }
        };
        let caption = |s: String, tone| text(s).size(ts(T::Caption)).style(kit::text(tone));
        column![text(label).size(ts(T::Label))].push(field.description.clone().map(|d| caption(d, Tone::Muted))).push(input).push(error.map(|e| caption(e.to_string(), Tone::Danger))).spacing(sp(Sp::Xs)).into()
    });
    column(fields).spacing(sp(Sp::M)).into()
}

/// `body` parsed as a unified diff; `None` when it is no parseable diff with lines.
fn parse_diff(body: &str) -> Option<Vec<FileDiff>> { parse_unified(body).ok().filter(|f| !f.is_empty() && f.iter().any(|f| !f.lines().is_empty())) }

/// A compact view of a parsed diff: each file's path, then its lines on a well with added and
/// removed lines tinted. Unless `full`, only the first [`PREVIEW_LINES`] rows render.
fn diff_view(files: &[FileDiff], full: bool) -> Element<'_, Message> {
    let limit = if full { usize::MAX } else { PREVIEW_LINES };
    let lines = files.iter().flat_map(|file| {
        let head = text(file.path()).font(Font::MONOSPACE).size(ts(T::Label)).style(kit::text(Tone::Muted)).into();
        std::iter::once(head).chain(file.lines().iter().enumerate().map(|(i, line)| {
            let (mark, tone, tint) = match line.tag {
                Tag::Insert => ('+', Tone::Ok, Some(C::AddedBg)),
                Tag::Delete => ('-', Tone::Danger, Some(C::RemovedBg)),
                Tag::Equal => (' ', Tone::Normal, None),
            };
            let line = text(format!("{mark}{}", file.line_text(i))).font(Font::MONOSPACE).size(ts(T::Label)).style(kit::text(tone));
            match tint {
                Some(role) => container(line).width(Fill).style(kit::fill(role, 0.0)).into(),
                None => line.into(),
            }
        }))
    });
    container(column(lines.take(limit).collect::<Vec<_>>())).padding([sp(Sp::Xs), sp(Sp::S)]).width(Fill).style(kit::container(Surface::Well)).into()
}
