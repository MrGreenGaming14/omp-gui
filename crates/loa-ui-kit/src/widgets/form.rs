//! Generic editing components: a form view over any [`Form`], validated [`FormState`], a list
//! editor, and a [`Picker`] over any [`PickerItem`].

use super::{label, menu_row, popover};
use crate::icons::{Icon, tinted};
use crate::style::{self, ButtonKind, Surface, Tone};
use crate::theme::{LayoutRole, SpaceRole, TextRole, tokens};
use iced::widget::{button, column, container, row, text_input};
use iced::{Alignment, Element, Fill};
use std::borrow::Cow;
use std::collections::BTreeMap;

/// One editable field of a [`Form`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub placeholder: &'static str,
}

/// Anything editable as labelled text fields (settings drafts, host limits).
pub trait Form {
    fn fields(&self) -> &[Field];
    fn get(&self, key: &str) -> String;
    fn set(&mut self, key: &str, value: &str) -> Result<(), String>;
}

/// Labelled inputs for every field of `form`, with `errors` under their fields.
pub fn form_view<'a, M: Clone + 'a>(form: &'a dyn Form, errors: &BTreeMap<&'static str, String>, on_edit: impl Fn(&'static str, String) -> M + 'a) -> Element<'a, M> {
    let (t, on_edit) = (tokens(), std::rc::Rc::new(on_edit));
    let rows = form.fields().iter().map(|f| {
        let on_edit = on_edit.clone();
        let input = text_input(f.placeholder, &form.get(f.key)).on_input(move |v| on_edit(f.key, v)).style(style::input);
        let mut field = column![input.padding(t.space(SpaceRole::S))];
        if let Some(e) = errors.get(f.key) { field = field.push(label(e.clone(), TextRole::Caption, Tone::Danger)) }
        let name = container(label(f.label, TextRole::Label, Tone::Muted)).width(t.layout(LayoutRole::LabelColumn));
        row![name, field.width(Fill)].spacing(t.space(SpaceRole::M)).align_y(Alignment::Center).into()
    });
    column(rows).spacing(t.space(SpaceRole::S)).into()
}

/// Validation for a [`FormState`].
pub trait FormSpec {
    type Field: Copy + Ord;
    type Output;
    fn validate(&self, existing: &[String]) -> Result<Self::Output, BTreeMap<Self::Field, String>>;
}

/// A form's values (`spec`) plus the errors of its last submit.
#[derive(Clone, Debug, Default)]
pub struct FormState<F: FormSpec> {
    pub spec: F,
    pub errors: BTreeMap<F::Field, String>,
}

impl<F: FormSpec> FormState<F> {
    pub fn new(spec: F) -> Self { Self { spec, errors: BTreeMap::new() } }
    pub fn error(&self, f: F::Field) -> Option<&str> { self.errors.get(&f).map(String::as_str) }
    /// The output when valid; otherwise records the errors.
    pub fn submit(&mut self, existing: &[String]) -> Option<F::Output> {
        let r = self.spec.validate(existing);
        self.errors = r.as_ref().err().cloned().unwrap_or_default();
        r.ok()
    }
}

/// Rows of `items` with a remove button each, then an input that adds `draft`.
pub fn list_editor<'a, T, M: Clone + 'a>(
    items: &'a [T],
    show: impl Fn(&T) -> String,
    on_remove: impl Fn(usize) -> M,
    draft: &'a str,
    on_draft: impl Fn(String) -> M + 'a,
    on_add: M,
) -> Element<'a, M> {
    let t = tokens();
    let remove = |i| button(tinted(Icon::X, TextRole::Caption, Tone::Muted)).style(style::button(ButtonKind::Ghost)).on_press(on_remove(i));
    let rows = items.iter().enumerate().map(|(i, it)| row![label(show(it), TextRole::Body, Tone::Normal).width(Fill), remove(i)].align_y(Alignment::Center).into());
    let input = text_input("", draft).on_input(on_draft).on_submit(on_add.clone()).style(style::input).padding(t.space(SpaceRole::S));
    let add = button(tinted(Icon::Plus, TextRole::Body, Tone::Normal)).style(style::button(ButtonKind::Secondary)).on_press(on_add);
    column(rows).push(row![input, add].spacing(t.space(SpaceRole::S))).spacing(t.space(SpaceRole::Xs)).into()
}

/// An option a [`Picker`] lists.
pub trait PickerItem {
    fn label(&self) -> Cow<'_, str>;
    fn detail(&self) -> Option<Cow<'_, str>> { None }
    fn icon(&self) -> Option<Icon> { None }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickerMsg {
    Toggle,
    Pick(usize),
}

/// A button showing the selected option that opens a menu of all of them above it.
#[derive(Clone, Debug, Default)]
pub struct Picker<T> {
    pub options: Vec<T>,
    pub selected: Option<usize>,
    pub open: bool,
}

impl<T: PickerItem> Picker<T> {
    pub fn new(options: Vec<T>, selected: Option<usize>) -> Self { Self { options, selected, open: false } }
    pub fn selected(&self) -> Option<&T> { self.options.get(self.selected?) }

    /// Applies `m`; returns the newly picked option.
    pub fn update(&mut self, m: PickerMsg) -> Option<&T> {
        match m {
            PickerMsg::Toggle => self.open = !self.open,
            PickerMsg::Pick(i) if i < self.options.len() => (self.selected, self.open) = (Some(i), false),
            PickerMsg::Pick(_) => {}
        }
        matches!(m, PickerMsg::Pick(_)).then(|| self.selected()).flatten()
    }

    pub fn view<'a, M: Clone + 'a>(&'a self, placeholder: &'a str, on: impl Fn(PickerMsg) -> M) -> Element<'a, M> {
        let t = tokens();
        let current = self.selected().map_or(Cow::Borrowed(placeholder), |s| s.label());
        let head = row![label(current, TextRole::Label, Tone::Normal), tinted(Icon::ChevronDown, TextRole::Caption, Tone::Muted)];
        let head = button(head.spacing(t.space(SpaceRole::Xs)).align_y(Alignment::Center)).style(style::button(ButtonKind::Ghost)).on_press(on(PickerMsg::Toggle));
        let menu = self.open.then(|| {
            let items = self.options.iter().enumerate().map(|(i, o)| {
                let text = match o.detail() { Some(d) => Cow::Owned(format!("{} · {d}", o.label())), None => o.label() };
                menu_row(o.icon(), text, self.selected == Some(i), on(PickerMsg::Pick(i)))
            });
            let menu = container(column(items).spacing(t.space(SpaceRole::Xs) / 2.0)).padding(t.space(SpaceRole::Xs)).width(t.layout(LayoutRole::PopupWidth) / 2.0);
            menu.style(style::container(Surface::Popup)).into()
        });
        popover(head, menu).into()
    }
}
