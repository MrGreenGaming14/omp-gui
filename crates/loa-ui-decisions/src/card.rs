//! One decision card: its local input state, the answers it produces, and
//! its view.

use crate::decision::{ApprovalChoice, Decision, PlanChoice, answer_summary};
use crate::form::{FieldType, FieldValue, Form, FormError};
use crate::{Message, TIMEOUT_BY, diff};
use iced::widget::{
    button, checkbox, column, container, markdown, pick_list, row, rule, text, text_editor,
    text_input,
};
use iced::{Element, Fill, Font, Theme};
use loa_core::{DecisionKind, DecisionStatus, NodeId};

/// Values and reason of an answer, before the answering user is added.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Reply {
    pub values: Vec<String>,
    pub reason: Option<String>,
}

impl Reply {
    fn one(value: impl Into<String>, reason: Option<String>) -> Self {
        Self {
            values: vec![value.into()],
            reason,
        }
    }

    /// Esc on a question or form: answered with nothing picked.
    fn skip() -> Self {
        Self {
            values: Vec::new(),
            reason: None,
        }
    }
}

/// Id of the deny reason, reject feedback or typed answer field of a card.
pub fn text_field_id(decision: NodeId) -> iced::widget::Id {
    iced::widget::Id::from(format!("decision-{decision}-text"))
}

/// Id of the text input of form field `index`.
pub fn form_field_id(decision: NodeId, index: usize) -> iced::widget::Id {
    iced::widget::Id::from(format!("decision-{decision}-field-{index}"))
}

/// What the user typed or picked into a form field.
#[derive(Clone, Debug, PartialEq)]
pub enum FieldInput {
    Text(String),
    Bool(bool),
    Choice(usize),
}

pub(crate) struct Card {
    pub decision: Decision,
    /// Answered locally, waiting for the DOM to record it.
    pub pending: bool,
    /// Option highlighted by a number key, confirmed by Enter.
    pub selected: Option<usize>,
    /// Multi choice questions: which options are ticked.
    pub picked: Vec<bool>,
    /// Deny reason, reject feedback or the typed question answer.
    pub text: String,
    plan: Vec<markdown::Item>,
    /// Plan being edited.
    pub editor: Option<text_editor::Content>,
    pub form: Option<Result<Form, FormError>>,
}

impl Card {
    pub fn new(decision: Decision) -> Self {
        let mut card = Self {
            picked: vec![false; decision.options.len()],
            plan: parse_plan(&decision),
            form: parse_form(&decision),
            decision,
            pending: false,
            selected: None,
            text: String::new(),
            editor: None,
        };
        card.close_if_settled();
        card
    }

    /// Takes the latest DOM state of the decision, keeping what the user
    /// entered while it waits.
    pub fn refresh(&mut self, decision: Decision) {
        if decision == self.decision {
            return;
        }
        if decision.body != self.decision.body {
            self.plan = parse_plan(&decision);
        }
        if decision.fields != self.decision.fields {
            self.form = parse_form(&decision);
        }
        if decision.options != self.decision.options {
            self.picked = vec![false; decision.options.len()];
            self.selected = None;
        }
        self.decision = decision;
        self.close_if_settled();
    }

    fn close_if_settled(&mut self) {
        if !self.decision.is_waiting() {
            self.pending = false;
            self.editor = None;
        }
    }

    /// Can take input from the local user.
    pub fn open(&self) -> bool {
        self.decision.is_waiting() && !self.pending
    }

    /// Number of options that number keys address.
    fn option_count(&self) -> usize {
        match self.decision.kind {
            DecisionKind::Approval => ApprovalChoice::ALL.len(),
            DecisionKind::Plan if self.editor.is_none() => PlanChoice::ALL.len(),
            DecisionKind::Question => self.decision.options.len(),
            DecisionKind::Plan | DecisionKind::Form => 0,
        }
    }

    /// Number key: highlight option `index` (toggle it on a multi choice
    /// question). Enter confirms.
    pub fn pick(&mut self, index: usize) {
        if index >= self.option_count() {
            return;
        }
        if self.decision.kind == DecisionKind::Question && self.decision.multi {
            self.picked[index] = !self.picked[index];
        }
        self.selected = Some(index);
    }

    /// Click on option `index`.
    pub fn choose(&mut self, index: usize) -> Option<Reply> {
        if index >= self.option_count() {
            return None;
        }
        match self.decision.kind {
            DecisionKind::Approval => Some(self.approval(ApprovalChoice::ALL[index])),
            DecisionKind::Plan => self.plan_choice(PlanChoice::ALL[index]),
            DecisionKind::Question if self.decision.multi => {
                self.picked[index] = !self.picked[index];
                self.selected = Some(index);
                None
            }
            DecisionKind::Question => Some(Reply::one(self.decision.options[index].clone(), None)),
            DecisionKind::Form => None,
        }
    }

    /// Enter, or the card's submit button.
    pub fn confirm(&mut self) -> Option<Reply> {
        match self.decision.kind {
            DecisionKind::Approval => self.selected.map(|i| self.approval(ApprovalChoice::ALL[i])),
            DecisionKind::Plan => match &self.editor {
                Some(_) => self.submit_edit(),
                None => self
                    .selected
                    .and_then(|i| self.plan_choice(PlanChoice::ALL[i])),
            },
            DecisionKind::Question if self.decision.multi && self.picked.contains(&true) => {
                let values = self
                    .decision
                    .options
                    .iter()
                    .zip(&self.picked)
                    .filter(|(_, picked)| **picked)
                    .map(|(option, _)| option.clone())
                    .collect();
                Some(Reply {
                    values,
                    reason: None,
                })
            }
            DecisionKind::Question if !self.decision.multi && self.selected.is_some() => self
                .selected
                .map(|i| Reply::one(self.decision.options[i].clone(), None)),
            DecisionKind::Question => self.submit_typed(),
            DecisionKind::Form => match &mut self.form {
                Some(Ok(form)) => form.submit().map(|json| Reply::one(json, None)),
                _ => None,
            },
        }
    }

    /// Esc: deny an approval, reject a plan (or leave plan editing), skip a
    /// question or form.
    pub fn dismiss(&mut self) -> Option<Reply> {
        match self.decision.kind {
            DecisionKind::Approval => Some(self.approval(ApprovalChoice::Deny)),
            DecisionKind::Plan if self.editor.is_some() => {
                self.editor = None;
                None
            }
            DecisionKind::Plan => self.plan_choice(PlanChoice::Reject),
            DecisionKind::Question | DecisionKind::Form => Some(Reply::skip()),
        }
    }

    /// Enter in the text field: deny with the reason, reject with the
    /// feedback, or answer the typed text.
    pub fn submit_text(&mut self) -> Option<Reply> {
        match self.decision.kind {
            DecisionKind::Approval => Some(self.approval(ApprovalChoice::Deny)),
            DecisionKind::Plan => self.plan_choice(PlanChoice::Reject),
            DecisionKind::Question => self.submit_typed(),
            DecisionKind::Form => None,
        }
    }

    pub fn submit_edit(&mut self) -> Option<Reply> {
        let edited = self.editor.as_ref()?.text();
        Some(Reply::one(PlanChoice::Edit.answer(), Some(edited)))
    }

    pub fn edit(&mut self, action: text_editor::Action) {
        if let Some(editor) = &mut self.editor {
            editor.perform(action);
        }
    }

    pub fn set_field(&mut self, index: usize, input: FieldInput) {
        if let Some(Ok(form)) = &mut self.form {
            form.set(
                index,
                match input {
                    FieldInput::Text(t) => FieldValue::Text(t),
                    FieldInput::Bool(b) => FieldValue::Bool(b),
                    FieldInput::Choice(i) => FieldValue::Choice(Some(i)),
                },
            );
        }
    }

    fn reason(&self) -> Option<String> {
        let trimmed = self.text.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }

    fn approval(&self, choice: ApprovalChoice) -> Reply {
        let reason = if choice == ApprovalChoice::Deny {
            self.reason()
        } else {
            None
        };
        Reply::one(choice.answer(), reason)
    }

    fn plan_choice(&mut self, choice: PlanChoice) -> Option<Reply> {
        match choice {
            PlanChoice::Approve => Some(Reply::one(choice.answer(), None)),
            PlanChoice::Edit => {
                self.editor = Some(text_editor::Content::with_text(&self.decision.body));
                self.selected = None;
                None
            }
            PlanChoice::Reject => Some(Reply::one(choice.answer(), self.reason())),
        }
    }

    fn submit_typed(&self) -> Option<Reply> {
        self.reason().map(|typed| Reply::one(typed, None))
    }

    pub fn view<'a>(
        &'a self,
        focused: bool,
        view_only: bool,
        markdown: markdown::Settings,
    ) -> Element<'a, Message> {
        let d = &self.decision;
        let id = d.id;
        // `Some` only when the local user may act on the card.
        let active = self.open() && !view_only;
        let on = |message: Message| active.then_some(message);

        let kind = match d.kind {
            DecisionKind::Approval => "Approval",
            DecisionKind::Question => "Question",
            DecisionKind::Plan => "Plan review",
            DecisionKind::Form => "Form",
        };
        let mut header = row![text(kind).style(text::primary)].spacing(8);
        if let Some(tool) = &d.tool {
            header = header.push(text(tool).font(Font::MONOSPACE));
        }
        if let Some(title) = &d.title {
            header = header.push(text(title));
        }
        let mut body = column![header].spacing(10);

        match d.kind {
            DecisionKind::Approval => {
                let preview: Element<'a, Message> = if diff::is_unified_diff(&d.body) {
                    diff::view(&d.body)
                } else {
                    text(&d.body).font(Font::MONOSPACE).into()
                };
                body = body.push(
                    container(preview)
                        .padding(8)
                        .width(Fill)
                        .style(container::rounded_box),
                );
                if d.is_waiting() {
                    let buttons = ApprovalChoice::ALL
                        .iter()
                        .enumerate()
                        .map(|(i, c)| self.choice_button(i, c.label(), on(Message::Choose(id, i))));
                    body = body.push(row(buttons).spacing(8));
                    body = body.push(
                        text_input("Reason for denying (optional)", &self.text)
                            .id(text_field_id(id))
                            .on_input_maybe(active.then_some(move |t| Message::Text(id, t)))
                            .on_submit_maybe(on(Message::SubmitText(id))),
                    );
                }
            }
            DecisionKind::Question => {
                body = body.push(text(&d.body));
                if d.is_waiting() {
                    let options = d.options.iter().enumerate().map(|(i, option)| {
                        if d.multi {
                            checkbox(self.picked[i])
                                .label(format!("{} {option}", i + 1))
                                .on_toggle_maybe(active.then_some(move |_| Message::Choose(id, i)))
                                .into()
                        } else {
                            self.choice_button(i, option, on(Message::Choose(id, i)))
                        }
                    });
                    body = body.push(column(options).spacing(6));
                    let mut answer_row = row![
                        text_input("Type your own answer", &self.text)
                            .id(text_field_id(id))
                            .on_input_maybe(active.then_some(move |t| Message::Text(id, t)))
                            .on_submit_maybe(on(Message::SubmitText(id))),
                        button("Send").on_press_maybe(on(Message::SubmitText(id))),
                    ]
                    .spacing(8);
                    if d.multi {
                        answer_row = answer_row
                            .push(button("Submit").on_press_maybe(on(Message::Submit(id))));
                    }
                    answer_row = answer_row.push(
                        button("Skip")
                            .style(button::secondary)
                            .on_press_maybe(on(Message::Skip(id))),
                    );
                    body = body.push(answer_row);
                }
            }
            DecisionKind::Plan => match &self.editor {
                Some(editor) => {
                    let editor = text_editor(editor).height(320).font(Font::MONOSPACE);
                    // Without `on_action` the editor is read only.
                    let editor = if active {
                        editor.on_action(move |a| Message::EditPlan(id, a))
                    } else {
                        editor
                    };
                    body = body.push(editor);
                    body = body.push(
                        row![
                            button("Submit edited plan").on_press_maybe(on(Message::Submit(id))),
                            button("Cancel")
                                .style(button::secondary)
                                .on_press_maybe(on(Message::CancelEdit(id))),
                        ]
                        .spacing(8),
                    );
                }
                None => {
                    body = body.push(markdown::view(&self.plan, markdown).map(Message::Link));
                    if d.is_waiting() {
                        let buttons = PlanChoice::ALL.iter().enumerate().map(|(i, c)| {
                            self.choice_button(i, c.label(), on(Message::Choose(id, i)))
                        });
                        body = body.push(row(buttons).spacing(8));
                        body = body.push(
                            text_input("Feedback for rejecting", &self.text)
                                .id(text_field_id(id))
                                .on_input_maybe(active.then_some(move |t| Message::Text(id, t)))
                                .on_submit_maybe(on(Message::SubmitText(id))),
                        );
                    }
                }
            },
            DecisionKind::Form => {
                if !d.body.is_empty() {
                    body = body.push(text(&d.body));
                }
                match &self.form {
                    Some(Ok(form)) if d.is_waiting() => {
                        body = body.push(form_view(id, form, active));
                        body = body.push(
                            row![
                                button("Submit").on_press_maybe(on(Message::Submit(id))),
                                button("Skip")
                                    .style(button::secondary)
                                    .on_press_maybe(on(Message::Skip(id))),
                            ]
                            .spacing(8),
                        );
                    }
                    Some(Err(error)) if d.is_waiting() => {
                        body = body.push(
                            text(format!("This form can't be shown: {error}")).style(text::danger),
                        );
                        body = body.push(
                            button("Skip")
                                .style(button::secondary)
                                .on_press_maybe(on(Message::Skip(id))),
                        );
                    }
                    None if d.is_waiting() => {
                        body = body.push(text("This form declares no fields").style(text::danger));
                        body = body.push(
                            button("Skip")
                                .style(button::secondary)
                                .on_press_maybe(on(Message::Skip(id))),
                        );
                    }
                    _ => {}
                }
            }
        }

        if let Some(footer) = self.footer(view_only) {
            body = body.push(rule::horizontal(1)).push(footer);
        }

        container(body)
            .padding(12)
            .width(Fill)
            .style(move |theme: &Theme| {
                let mut style = container::bordered_box(theme);
                if focused {
                    style.border.color = theme.extended_palette().primary.strong.color;
                    style.border.width = 2.0;
                }
                style
            })
            .into()
    }

    fn choice_button<'a>(
        &self,
        index: usize,
        label: &'a str,
        on_press: Option<Message>,
    ) -> Element<'a, Message> {
        let selected = self.selected == Some(index);
        button(row![text(index + 1).font(Font::MONOSPACE), text(label)].spacing(8))
            .style(if selected {
                button::primary
            } else {
                button::secondary
            })
            .on_press_maybe(on_press)
            .into()
    }

    /// Status line: who answered, why it closed, or why it can't be clicked.
    fn footer<'a>(&'a self, view_only: bool) -> Option<Element<'a, Message>> {
        let d = &self.decision;
        let by = |prefix: &str| -> String {
            match d.by.as_deref() {
                Some(TIMEOUT_BY) => format!("{prefix} by timeout (default answer)"),
                Some(by) => format!("{prefix} by {by}"),
                None => prefix.to_owned(),
            }
        };
        let status = match d.status {
            DecisionStatus::Waiting if self.pending => "Sending answer…".to_owned(),
            DecisionStatus::Waiting if view_only => {
                "View only: you can't answer this prompt".to_owned()
            }
            DecisionStatus::Waiting => return None,
            DecisionStatus::Answered => by(&answer_summary(d.kind, &d.answer)),
            DecisionStatus::Expired => by("Expired"),
            DecisionStatus::Cancelled => "Cancelled".to_owned(),
        };
        let mut footer = column![text(status).style(text::secondary)].spacing(6);
        if d.status == DecisionStatus::Answered {
            if d.kind == DecisionKind::Form
                && let [json] = d.answer.as_slice()
            {
                footer = footer.push(text(json).font(Font::MONOSPACE));
            }
            if let Some(reason) = &d.reason {
                let reason: Element<'a, Message> = if d.kind == DecisionKind::Plan
                    && PlanChoice::from_answer(d.answer.first().map_or("", String::as_str))
                        == Some(PlanChoice::Edit)
                {
                    text(reason).font(Font::MONOSPACE).into()
                } else {
                    text(reason).into()
                };
                footer = footer.push(reason);
            }
        }
        Some(footer.into())
    }
}

fn form_view<'a>(id: NodeId, form: &'a Form, active: bool) -> Element<'a, Message> {
    let fields = form
        .fields
        .iter()
        .zip(&form.values)
        .zip(&form.errors)
        .enumerate()
        .map(|(i, ((field, value), error))| {
            let label = if field.required {
                format!("{} *", field.label)
            } else {
                field.label.clone()
            };
            let mut cell = column![text(label)].spacing(4);
            if let Some(description) = &field.description {
                cell = cell.push(text(description).size(13).style(text::secondary));
            }
            let input: Element<'a, Message> = match value {
                FieldValue::Bool(checked) => checkbox(*checked)
                    .label(field.label.as_str())
                    .on_toggle_maybe(
                        active.then_some(move |b| Message::Field(id, i, FieldInput::Bool(b))),
                    )
                    .into(),
                FieldValue::Choice(choice) => {
                    let labels = match &field.ty {
                        FieldType::Enum { labels, .. } => labels.as_slice(),
                        _ => &[],
                    };
                    let selected = choice.and_then(|c| labels.get(c));
                    if active {
                        pick_list(labels, selected, move |label: String| {
                            // The label comes from `labels`, so the position exists.
                            let index = labels.iter().position(|l| *l == label).unwrap_or_default();
                            Message::Field(id, i, FieldInput::Choice(index))
                        })
                        .into()
                    } else {
                        text(selected.map_or("", String::as_str)).into()
                    }
                }
                FieldValue::Text(raw) => text_input(&field.name, raw)
                    .id(form_field_id(id, i))
                    .on_input_maybe(
                        active.then_some(move |t| Message::Field(id, i, FieldInput::Text(t))),
                    )
                    .on_submit_maybe(active.then_some(Message::Submit(id)))
                    .into(),
            };
            cell = cell.push(input);
            if let Some(error) = error {
                cell = cell.push(text(error.to_string()).style(text::danger));
            }
            cell.into()
        });
    column(fields).spacing(12).into()
}

fn parse_plan(decision: &Decision) -> Vec<markdown::Item> {
    if decision.kind == DecisionKind::Plan {
        markdown::parse(&decision.body).collect()
    } else {
        Vec::new()
    }
}

fn parse_form(decision: &Decision) -> Option<Result<Form, FormError>> {
    if decision.kind != DecisionKind::Form {
        return None;
    }
    decision.fields.as_deref().map(Form::parse)
}
