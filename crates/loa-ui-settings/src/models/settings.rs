//! Model settings: model roles with their fallback chains, enabled models, provider order, per
//! model overrides (a kit form over omp's [`OVERRIDE_FIELDS`]), custom providers and the other
//! harnesses' own default models. The app loads [`ModelSettings`], writes what [`Event::Save`]
//! carries and answers with [`State::saved`] or [`State::set_errors`].

use crate::{action, check, errors, field, labeled, pick, section};
use iced::widget::{Column, column, container, row};
use iced::{Alignment, Element, Fill};
use loa_models::models_yml::OverrideKey;
use loa_models::settings::{AuthMode, CustomProvider, FieldValue, ModelOverride, ModelSettings, OVERRIDE_FIELDS, OverrideField, RoleChain, ValidationError, is_env_var_name, is_provider_id};
use loa_ui_kit::style::{self, ButtonKind, Surface, Tone};
use loa_ui_kit::theme::{SpaceRole, TextRole, sp};
use loa_ui_kit::widgets::{Field, Form, Msg, Screen, form_view, label, list_editor, reorder};
use std::collections::BTreeMap;
use std::sync::LazyLock;

/// The effort list's entry for "no effort set".
const HARNESS_DEFAULT: &str = "default";

type Errors = BTreeMap<&'static str, String>;

/// One kit field per editable override.
static OVERRIDE_FORM: LazyLock<Vec<Field>> = LazyLock::new(|| {
    let placeholder = |k| match k { OverrideKey::ContextWindow | OverrideKey::MaxContextWindow | OverrideKey::MaxTokens => "whole number", OverrideKey::Reasoning | OverrideKey::PreferWebsockets => "true or false", _ => "" };
    OVERRIDE_FIELDS.iter().map(|&(key, k)| Field { key, label: key, placeholder: placeholder(k) }).collect()
});

const PROVIDER_FORM: [Field; 5] = [
    Field { key: "id", label: "Id", placeholder: "my-provider" },
    Field { key: "base_url", label: "Base URL", placeholder: "https://example.com/v1" },
    Field { key: "api", label: "API", placeholder: "openai-completions" },
    Field { key: "api_key_env", label: "API key variable", placeholder: "MY_PROVIDER_API_KEY" },
    Field { key: "models", label: "Models", placeholder: "model-a, model-b" },
];

/// `raw` as the value of override `key`; empty leaves it unset.
fn parse(key: OverrideKey, raw: &str) -> Result<Option<FieldValue>, String> {
    let raw = raw.trim();
    if raw.is_empty() { return Ok(None) }
    Ok(Some(match key {
        OverrideKey::ContextWindow | OverrideKey::MaxContextWindow | OverrideKey::MaxTokens => FieldValue::Int(raw.parse().ok().filter(|n| *n > 0).ok_or_else(|| format!("\"{raw}\" is not a whole number above zero"))?),
        OverrideKey::Reasoning | OverrideKey::PreferWebsockets => FieldValue::Bool(raw.parse().map_err(|_| format!("\"{raw}\" is not true or false"))?),
        _ => FieldValue::Text(raw.to_owned()),
    }))
}

/// The value of field `key` among `values`, which are parallel to `fields`.
fn get(fields: &[Field], values: &[String], key: &str) -> String { fields.iter().position(|f| f.key == key).map_or_else(String::new, |i| values[i].clone()) }

fn set(fields: &[Field], values: &mut [String], key: &str, value: &str) -> Result<(), String> {
    let i = fields.iter().position(|f| f.key == key).ok_or_else(|| format!("no field {key}"))?;
    values[i] = value.to_owned();
    Ok(())
}

/// An override's values as typed, parallel to [`OVERRIDE_FIELDS`].
#[derive(Clone, Debug, PartialEq)]
struct OverrideDraft {
    selector: String,
    values: Vec<String>,
}

impl OverrideDraft {
    fn new(o: &ModelOverride) -> Self {
        let raw = |key: &str| o.fields.iter().find(|f| f.key == key).map_or_else(String::new, |f| match &f.value {
            FieldValue::Text(s) => s.clone(),
            FieldValue::Int(n) => n.to_string(),
            FieldValue::Float(n) => n.to_string(),
            FieldValue::Bool(b) => b.to_string(),
        });
        Self { selector: o.selector.clone(), values: OVERRIDE_FIELDS.iter().map(|(key, _)| raw(key)).collect() }
    }

    /// The typed values as override fields, and the ones that don't parse.
    fn parsed(&self) -> (Vec<OverrideField>, Errors) {
        let (mut fields, mut errors) = (Vec::new(), Errors::new());
        for (&(key, k), raw) in OVERRIDE_FIELDS.iter().zip(&self.values) {
            match parse(k, raw) {
                Ok(value) => fields.extend(value.map(|value| OverrideField { key: key.to_owned(), value })),
                Err(e) => {
                    errors.insert(key, e);
                }
            }
        }
        (fields, errors)
    }
}

impl Form for OverrideDraft {
    fn fields(&self) -> &[Field] { &OVERRIDE_FORM }
    fn get(&self, key: &str) -> String { get(&OVERRIDE_FORM, &self.values, key) }
    fn set(&mut self, key: &str, value: &str) -> Result<(), String> { set(&OVERRIDE_FORM, &mut self.values, key, value) }
}

/// A custom provider as typed, parallel to [`PROVIDER_FORM`]; models are comma separated.
#[derive(Clone, Debug, PartialEq)]
struct ProviderDraft {
    values: [String; 5],
    auth: AuthMode,
}

impl ProviderDraft {
    fn new(p: &CustomProvider) -> Self { Self { values: [p.id.clone(), p.base_url.clone(), p.api.clone(), p.api_key_env.clone(), p.models.join(", ")], auth: p.auth } }
    fn provider(&self) -> CustomProvider {
        let [id, base_url, api, api_key_env, models] = self.values.each_ref().map(|v| v.trim().to_owned());
        let models = models.split(',').map(str::trim).filter(|m| !m.is_empty()).map(str::to_owned).collect();
        CustomProvider { id, base_url, api, auth: self.auth, api_key_env, models }
    }
}

impl Form for ProviderDraft {
    fn fields(&self) -> &[Field] { &PROVIDER_FORM }
    fn get(&self, key: &str) -> String { get(&PROVIDER_FORM, &self.values, key) }
    fn set(&mut self, key: &str, value: &str) -> Result<(), String> { set(&PROVIDER_FORM, &mut self.values, key, value) }
}

/// Form problems of a custom provider among `others` that omp's own validation would only
/// report less clearly. An API key pasted as its variable is refused, so it never reaches a file.
fn provider_errors(p: &CustomProvider, others: &[CustomProvider]) -> Errors {
    let mut errors = Errors::new();
    let id = p.id.as_str();
    let id_error = if id.is_empty() {
        Some("Provider id is empty".to_owned())
    } else if !is_provider_id(id) || id.chars().any(|c| c.is_ascii_uppercase()) {
        Some("Provider id may only contain a-z, 0-9, -, _ and .".to_owned())
    } else {
        others.iter().any(|o| o.id == id).then(|| format!("Another custom provider already uses the id \"{id}\""))
    };
    errors.extend(id_error.map(|e| ("id", e)));
    if !p.api_key_env.is_empty() && !is_env_var_name(&p.api_key_env) {
        errors.insert("api_key_env", "API key must be the name of an environment variable (A-Z, 0-9, _), not the key itself".into());
    }
    errors
}

/// The editable form of [`ModelSettings`]: `base` holds roles, toggles and harnesses; overrides
/// and custom providers stay as typed text.
#[derive(Clone, Debug, PartialEq)]
struct Draft {
    base: ModelSettings,
    overrides: Vec<OverrideDraft>,
    custom: Vec<ProviderDraft>,
}

impl Draft {
    fn new(s: &ModelSettings) -> Self {
        let base = ModelSettings { overrides: Vec::new(), custom_providers: Vec::new(), ..s.clone() };
        Self { base, overrides: s.overrides.iter().map(OverrideDraft::new).collect(), custom: s.custom_providers.iter().map(ProviderDraft::new).collect() }
    }

    fn providers(&self) -> Vec<CustomProvider> { self.custom.iter().map(ProviderDraft::provider).collect() }

    /// Each provider's form errors.
    fn provider_errors(&self) -> Vec<Errors> {
        let providers = self.providers();
        (0..providers.len()).map(|i| provider_errors(&providers[i], &[&providers[..i], &providers[i + 1..]].concat())).collect()
    }

    /// The settings, or every problem keeping them from being saved.
    fn settings(&self) -> Result<ModelSettings, Vec<String>> {
        let mut problems = Vec::new();
        let roles: Vec<RoleChain> = self.base.roles.iter().map(|r| RoleChain { role: r.role.trim().to_owned(), chain: r.chain.clone() }).collect();
        for (i, role) in roles.iter().enumerate() {
            match role.role.as_str() {
                "" => problems.push(format!("Role {} has no name", i + 1)),
                name if roles[..i].iter().any(|r| r.role == name) => problems.push(format!("Role \"{name}\" is listed twice")),
                _ => {}
            }
        }
        let mut overrides = Vec::with_capacity(self.overrides.len());
        for o in &self.overrides {
            let (fields, errors) = o.parsed();
            problems.extend(errors.into_iter().map(|(key, e)| format!("{}: {key}: {e}", o.selector)));
            overrides.push(ModelOverride { selector: o.selector.clone(), fields });
        }
        let custom_providers = self.providers();
        for (i, (p, errors)) in custom_providers.iter().zip(self.provider_errors()).enumerate() {
            let name = if p.id.is_empty() { format!("Custom provider {}", i + 1) } else { p.id.clone() };
            problems.extend(errors.into_values().map(|e| format!("{name}: {e}")));
        }
        if !problems.is_empty() { return Err(problems) }
        Ok(ModelSettings { roles, overrides, custom_providers, ..self.base.clone() })
    }
}

#[derive(Clone, Debug)]
pub enum Local {
    NewRole(String),
    AddRole,
    RemoveRole(usize),
    RenameRole(usize, String),
    /// Text typed into a role's chain input.
    ChainInput(usize, String),
    /// Appends a picked selector to a role's chain, or the typed one with `None`.
    AddChain(usize, Option<String>),
    RemoveChain(usize, usize),
    /// Moves a role's chain entry: role, from, to.
    MoveChain(usize, usize, usize),
    ModelToggled(usize, bool),
    ProviderToggled(usize, bool),
    MoveProvider(usize, usize),
    NewOverride(String),
    /// Overrides a picked model, or the typed one with `None`.
    AddOverride(Option<String>),
    RemoveOverride(usize),
    OverrideEdit(usize, &'static str, String),
    AddProvider,
    RemoveProvider(usize),
    ProviderEdit(usize, &'static str, String),
    ProviderAuth(usize, AuthMode),
    HarnessModel(usize, String),
    /// [`HARNESS_DEFAULT`] clears the effort.
    HarnessEffort(usize, String),
    Save,
    Revert,
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Write these settings, then call [`State::saved`] or [`State::set_errors`].
    Save(ModelSettings),
    /// The unsaved changes were dropped.
    Reverted,
}

pub type Message = Msg<Local, Event>;

#[derive(Clone, Debug)]
pub struct State {
    baseline: Draft,
    draft: Draft,
    /// Selectors offered when adding chain entries and overrides.
    known: Vec<String>,
    new_role: String,
    /// Each role's chain input, parallel to its roles.
    chain_inputs: Vec<String>,
    new_override: String,
    /// Problems the last Save found in the draft.
    local_errors: Vec<String>,
    /// Problems the app reported for the last write.
    app_errors: Vec<ValidationError>,
}

impl State {
    pub fn new(settings: ModelSettings, known: Vec<String>) -> Self { Self::from_draft(Draft::new(&settings), known) }

    /// `draft` as both baseline and draft, every input and error cleared.
    fn from_draft(draft: Draft, known: Vec<String>) -> Self {
        let chain_inputs = vec![String::new(); draft.base.roles.len()];
        Self { baseline: draft.clone(), draft, known, new_role: String::new(), chain_inputs, new_override: String::new(), local_errors: Vec::new(), app_errors: Vec::new() }
    }

    /// The app wrote `settings`: they become the baseline and the draft.
    pub fn saved(&mut self, settings: ModelSettings) { *self = Self::from_draft(Draft::new(&settings), std::mem::take(&mut self.known)) }

    /// Shows what the app (or omp) reported for the last write.
    pub fn set_errors(&mut self, errors: Vec<ValidationError>) { self.app_errors = errors }
    pub fn errors(&self) -> &[ValidationError] { &self.app_errors }
    pub fn local_errors(&self) -> &[String] { &self.local_errors }
    pub fn is_dirty(&self) -> bool { self.draft != self.baseline }
    pub fn draft_settings(&self) -> Result<ModelSettings, Vec<String>> { self.draft.settings() }

    pub fn view(&self) -> Element<'_, Message> {
        let dirty = self.is_dirty();
        let local = |l| dirty.then_some(Msg::Local(l));
        let head = row![label("Model settings", TextRole::Body, Tone::Normal), iced::widget::Space::new().width(Fill)].push(dirty.then(|| label("Unsaved changes", TextRole::Caption, Tone::Muted)));
        let head = head.push(action("Revert", ButtonKind::Ghost, local(Local::Revert))).push(action("Save", ButtonKind::Primary, local(Local::Save)));
        let app_errors = (!self.app_errors.is_empty()).then(|| errors(self.app_errors.iter().map(|e| format!("{}: {}", e.location, e.message))));
        let local_errors = (!self.local_errors.is_empty()).then(|| errors(self.local_errors.iter().cloned()));
        let sections = [
            section("Model roles", self.roles()),
            section("Enabled models", self.models()),
            section("Providers (order)", self.providers()),
            section("Model overrides", self.overrides()),
            section("Custom providers", self.custom_providers()),
            section("Other harnesses", self.harnesses()),
        ];
        crate::scroll(column![head.spacing(sp(SpaceRole::S)).align_y(Alignment::Center), app_errors, local_errors].extend(sections).spacing(sp(SpaceRole::L)))
    }

    fn roles(&self) -> Element<'_, Message> {
        let roles = self.draft.base.roles.iter().enumerate().map(|(i, role)| {
            let name = row![field("Role name", &role.role, move |n| Msg::Local(Local::RenameRole(i, n))), action("Remove role", ButtonKind::Danger, Some(Msg::Local(Local::RemoveRole(i))))].spacing(sp(SpaceRole::S));
            let moves = move |from, to| Msg::Local(Local::MoveChain(i, from, to));
            let chain = list_editor(&role.chain, Clone::clone, move |j| Msg::Local(Local::RemoveChain(i, j)), Some(&moves), self.chain_inputs.get(i).map_or("", String::as_str), move |v| Msg::Local(Local::ChainInput(i, v)), Msg::Local(Local::AddChain(i, None)));
            let add = pick(self.known.as_slice(), None, "Add model", move |s| Msg::Local(Local::AddChain(i, Some(s))));
            let hint = label("The first model is the primary, the rest its fallbacks in order; type provider/model[:thinking] to add one", TextRole::Caption, Tone::Faint);
            card(column![name, hint, chain, add])
        });
        let add = row![field("New role name", &self.new_role, |n| Msg::Local(Local::NewRole(n))).on_submit(Msg::Local(Local::AddRole)), action("Add role", ButtonKind::Secondary, Some(Msg::Local(Local::AddRole)))];
        Column::with_children(roles).push(add.spacing(sp(SpaceRole::S))).spacing(sp(SpaceRole::M)).into()
    }

    fn models(&self) -> Element<'_, Message> {
        let models = &self.draft.base.models;
        if models.is_empty() { return label("No models available", TextRole::Label, Tone::Muted).into() }
        column(models.iter().enumerate().map(|(i, m)| check(m.enabled, m.selector.as_str(), move |on| Msg::Local(Local::ModelToggled(i, on))))).spacing(sp(SpaceRole::Xs)).into()
    }

    fn providers(&self) -> Element<'_, Message> {
        let providers = &self.draft.base.providers;
        if providers.is_empty() { return label("No providers available", TextRole::Label, Tone::Muted).into() }
        let moves = |from, to| Msg::Local(Local::MoveProvider(from, to));
        column(providers.iter().enumerate().map(|(i, p)| {
            let name = if p.name.is_empty() { p.id.clone() } else { format!("{} ({})", p.name, p.id) };
            row![container(check(p.enabled, name, move |on| Msg::Local(Local::ProviderToggled(i, on)))).width(Fill), reorder(i, providers.len(), &moves)].align_y(Alignment::Center).into()
        }))
        .spacing(sp(SpaceRole::Xs))
        .into()
    }

    fn overrides(&self) -> Element<'_, Message> {
        let overrides = self.draft.overrides.iter().enumerate().map(|(i, o)| {
            let head = row![label(o.selector.as_str(), TextRole::Mono, Tone::Normal).width(Fill), action("Remove override", ButtonKind::Danger, Some(Msg::Local(Local::RemoveOverride(i))))];
            card(column![head.align_y(Alignment::Center), form_view(o, &o.parsed().1, move |k, v| Msg::Local(Local::OverrideEdit(i, k, v)), None)])
        });
        let add = row![
            pick(self.known.as_slice(), None, "Override a model", |s| Msg::Local(Local::AddOverride(Some(s)))),
            field("or type provider/model", &self.new_override, |s| Msg::Local(Local::NewOverride(s))).on_submit(Msg::Local(Local::AddOverride(None))),
            action("Add", ButtonKind::Secondary, Some(Msg::Local(Local::AddOverride(None)))),
        ];
        Column::with_children(overrides).push(add.spacing(sp(SpaceRole::S)).align_y(Alignment::Center)).spacing(sp(SpaceRole::M)).into()
    }

    fn custom_providers(&self) -> Element<'_, Message> {
        let errors = self.draft.provider_errors();
        let providers = self.draft.custom.iter().zip(errors).enumerate().map(|(i, (p, errors))| {
            let form = form_view(p, &errors, move |k, v| Msg::Local(Local::ProviderEdit(i, k, v)), None);
            let auth = labeled("Auth", pick(AuthMode::ALL, Some(p.auth), "", move |a| Msg::Local(Local::ProviderAuth(i, a))));
            card(column![form, auth, action("Remove provider", ButtonKind::Danger, Some(Msg::Local(Local::RemoveProvider(i))))])
        });
        Column::with_children(providers).push(action("Add custom provider", ButtonKind::Secondary, Some(Msg::Local(Local::AddProvider)))).spacing(sp(SpaceRole::M)).into()
    }

    fn harnesses(&self) -> Element<'_, Message> {
        let harnesses = &self.draft.base.harnesses;
        if harnesses.is_empty() { return label("No other harness settings found", TextRole::Label, Tone::Muted).into() }
        column(harnesses.iter().enumerate().map(|(i, h)| {
            let efforts: Vec<String> = std::iter::once(HARNESS_DEFAULT.to_owned()).chain(h.efforts.iter().cloned()).collect();
            let effort = if h.effort.is_empty() { HARNESS_DEFAULT.to_owned() } else { h.effort.clone() };
            let name = row![label(h.name.as_str(), TextRole::Body, Tone::Normal), label(h.config_path.as_str(), TextRole::Mono, Tone::Muted)].spacing(sp(SpaceRole::S));
            let model = labeled("Model", field("harness default", &h.model, move |m| Msg::Local(Local::HarnessModel(i, m))));
            card(column![name, model, labeled("Effort", pick(efforts, Some(effort), "", move |e| Msg::Local(Local::HarnessEffort(i, e))))])
        }))
        .spacing(sp(SpaceRole::M))
        .into()
    }

    /// Back to the baseline, every input and error cleared.
    fn reset(&mut self) { *self = Self::from_draft(self.baseline.clone(), std::mem::take(&mut self.known)) }
}

fn card<'a>(content: Column<'a, Message>) -> Element<'a, Message> {
    container(content.spacing(sp(SpaceRole::S))).padding(sp(SpaceRole::S)).width(Fill).style(style::container(Surface::Card)).into()
}

/// Moves `items[from]` to `to`; out of range moves nothing.
fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) {
    if from < items.len() && to < items.len() {
        let item = items.remove(from);
        items.insert(to, item);
    }
}

fn remove<T>(items: &mut Vec<T>, i: usize) {
    if i < items.len() {
        items.remove(i);
    }
}

/// Applies `f` to `items[i]` when it exists.
fn at<T>(items: &mut [T], i: usize, f: impl FnOnce(&mut T)) {
    if let Some(item) = items.get_mut(i) {
        f(item);
    }
}

impl Screen for State {
    type Local = Local;
    type Event = Event;

    fn reduce(&mut self, m: Local) -> Option<Event> {
        let d = &mut self.draft;
        match m {
            Local::NewRole(name) => self.new_role = name,
            Local::AddRole => {
                let name = self.new_role.trim();
                if !name.is_empty() {
                    d.base.roles.push(RoleChain { role: name.to_owned(), chain: Vec::new() });
                    self.chain_inputs.push(String::new());
                    self.new_role.clear();
                }
            }
            Local::RemoveRole(i) => {
                remove(&mut d.base.roles, i);
                remove(&mut self.chain_inputs, i);
            }
            Local::RenameRole(i, name) => at(&mut d.base.roles, i, |r| r.role = name),
            Local::ChainInput(i, value) => at(&mut self.chain_inputs, i, |input| *input = value),
            Local::AddChain(i, picked) => {
                let typed = || self.chain_inputs.get_mut(i).map(std::mem::take);
                let selector = picked.or_else(typed).unwrap_or_default();
                let selector = selector.trim();
                if let Some(role) = d.base.roles.get_mut(i).filter(|r| !selector.is_empty() && !r.chain.iter().any(|s| s == selector)) {
                    role.chain.push(selector.to_owned());
                }
            }
            Local::RemoveChain(i, j) => at(&mut d.base.roles, i, |r| remove(&mut r.chain, j)),
            Local::MoveChain(i, from, to) => at(&mut d.base.roles, i, |r| move_item(&mut r.chain, from, to)),
            Local::ModelToggled(i, on) => at(&mut d.base.models, i, |m| m.enabled = on),
            Local::ProviderToggled(i, on) => at(&mut d.base.providers, i, |p| p.enabled = on),
            Local::MoveProvider(from, to) => move_item(&mut d.base.providers, from, to),
            Local::NewOverride(s) => self.new_override = s,
            Local::AddOverride(picked) => {
                let selector = picked.unwrap_or_else(|| std::mem::take(&mut self.new_override));
                let selector = selector.trim();
                if !selector.is_empty() && !d.overrides.iter().any(|o| o.selector == selector) {
                    d.overrides.push(OverrideDraft { selector: selector.to_owned(), values: vec![String::new(); OVERRIDE_FIELDS.len()] });
                }
            }
            Local::RemoveOverride(i) => remove(&mut d.overrides, i),
            Local::OverrideEdit(i, key, value) => at(&mut d.overrides, i, |o| {
                let _ = o.set(key, &value);
            }),
            Local::AddProvider => d.custom.push(ProviderDraft::new(&CustomProvider::default())),
            Local::RemoveProvider(i) => remove(&mut d.custom, i),
            Local::ProviderEdit(i, key, value) => at(&mut d.custom, i, |p| {
                let _ = p.set(key, &value);
            }),
            Local::ProviderAuth(i, auth) => at(&mut d.custom, i, |p| p.auth = auth),
            Local::HarnessModel(i, model) => at(&mut d.base.harnesses, i, |h| h.model = model),
            Local::HarnessEffort(i, effort) => at(&mut d.base.harnesses, i, |h| h.effort = if effort == HARNESS_DEFAULT { String::new() } else { effort }),
            Local::Save => {
                self.app_errors.clear();
                match d.settings() {
                    Ok(settings) => {
                        self.local_errors.clear();
                        return Some(Event::Save(settings));
                    }
                    Err(problems) => self.local_errors = problems,
                }
            }
            Local::Revert => {
                self.reset();
                return Some(Event::Reverted);
            }
        }
        None
    }

    /// Every event comes from the reducer.
    fn guard(&self, _: &Event) -> bool { false }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_models::settings::{HarnessModel, ModelToggle, ProviderToggle};
    use loa_ui_kit::testkit::click_label;

    fn settings() -> ModelSettings {
        let provider = |id: &str, name: &str, enabled| ProviderToggle { id: id.into(), name: name.into(), enabled };
        ModelSettings {
            roles: vec![RoleChain { role: "default".into(), chain: vec!["anthropic/claude-sonnet-4-5:high".into(), "openai/gpt-5".into(), "google/gemini-2.5-pro".into()] }],
            models: vec![ModelToggle { selector: "openai/gpt-5".into(), enabled: true }],
            providers: vec![provider("anthropic", "Anthropic", true), provider("openai", "OpenAI", true), provider("google", "Google", false)],
            overrides: vec![ModelOverride { selector: "openai/gpt-5".into(), fields: vec![OverrideField { key: "contextWindow".into(), value: FieldValue::Int(400_000) }] }],
            custom_providers: Vec::new(),
            harnesses: vec![HarnessModel { harness: "codex".into(), name: "Codex".into(), config_path: "~/.codex/config.toml".into(), model: String::new(), effort: String::new(), efforts: vec!["low".into(), "high".into()] }],
        }
    }

    fn local_provider() -> CustomProvider {
        CustomProvider { id: "local-llm".into(), base_url: "http://localhost:8080/v1".into(), api: "openai-completions".into(), auth: AuthMode::ApiKey, api_key_env: "LOCAL_LLM_KEY".into(), models: vec!["qwen3".into(), "llama-4".into()] }
    }

    fn state() -> State { State::new(settings(), vec!["openai/gpt-5".into(), "anthropic/claude-haiku-4-5".into()]) }
    fn run(s: &mut State, messages: Vec<Local>) -> Option<Event> { messages.into_iter().map(|m| s.update(Msg::Local(m))).last().flatten() }
    fn saved(f: fn(&mut ModelSettings)) -> Option<Event> {
        let mut s = settings();
        f(&mut s);
        Some(Event::Save(s))
    }

    #[test]
    fn edits_save_as_settings() {
        use Local::*;
        let edit = |i, key, v: &str| ProviderEdit(i, key, v.into());
        // (messages, the event of the last one)
        let cases: Vec<(Vec<Local>, Option<Event>)> = vec![
            (vec![MoveChain(0, 2, 0), Save], saved(|s| s.roles[0].chain.rotate_right(1))),
            (vec![AddChain(0, Some("anthropic/claude-haiku-4-5".into())), ChainInput(0, " local/qwen3 ".into()), AddChain(0, None), AddChain(0, Some("openai/gpt-5".into())), RemoveChain(0, 1), Save],
                saved(|s| s.roles[0].chain = vec!["anthropic/claude-sonnet-4-5:high".into(), "google/gemini-2.5-pro".into(), "anthropic/claude-haiku-4-5".into(), "local/qwen3".into()])),
            (vec![MoveProvider(2, 1), MoveProvider(0, 1), MoveProvider(5, 0), Save], saved(|s| s.providers = vec![s.providers[2].clone(), s.providers[0].clone(), s.providers[1].clone()])),
            (vec![OverrideEdit(0, "contextWindow", "12x".into()), Save], None),
            (vec![OverrideEdit(0, "contextWindow", "128000".into()), OverrideEdit(0, "reasoning", "true".into()), Save],
                saved(|s| s.overrides[0].fields = vec![OverrideField { key: "contextWindow".into(), value: FieldValue::Int(128_000) }, OverrideField { key: "reasoning".into(), value: FieldValue::Bool(true) }])),
            (vec![AddOverride(Some("anthropic/claude-haiku-4-5".into())), AddOverride(Some("openai/gpt-5".into())), OverrideEdit(1, "name", " Haiku ".into()), OverrideEdit(1, "contextWindow", "1".into()), Save],
                saved(|s| s.overrides.push(ModelOverride { selector: "anthropic/claude-haiku-4-5".into(), fields: vec![OverrideField { key: "name".into(), value: FieldValue::Text("Haiku".into()) }, OverrideField { key: "contextWindow".into(), value: FieldValue::Int(1) }] }))),
            (vec![AddOverride(Some("a/b".into())), OverrideEdit(1, "reasoning", "maybe".into()), Save], None),
            (vec![NewRole("plan".into()), AddRole, RenameRole(1, "default".into()), Save], None),
            (vec![AddProvider, Save], None),
            (vec![AddProvider, edit(0, "id", "local-llm"), edit(0, "base_url", "http://localhost:8080/v1"), edit(0, "api", "openai-completions"), edit(0, "api_key_env", "LOCAL_LLM_KEY"), edit(0, "models", " qwen3 ,, llama-4 "), Save], saved(|s| s.custom_providers = vec![local_provider()])),
            (vec![HarnessModel(0, "o3".into()), HarnessEffort(0, "high".into()), Save], saved(|s| (s.harnesses[0].model, s.harnesses[0].effort) = ("o3".into(), "high".into()))),
            (vec![HarnessEffort(0, "high".into()), HarnessEffort(0, HARNESS_DEFAULT.into()), RemoveRole(0), Save], saved(|s| s.roles.clear())),
            (vec![ModelToggled(0, false), RenameRole(0, "smol".into()), Revert], Some(Event::Reverted)),
        ];
        for (i, (messages, want)) in cases.into_iter().enumerate() {
            let mut s = state();
            assert_eq!(run(&mut s, messages), want, "case {i}: {:?}", s.local_errors);
            assert_eq!(s.local_errors.is_empty(), want.is_some(), "case {i}: {:?}", s.local_errors);
        }
    }

    #[test]
    fn custom_providers_refuse_bad_ids_and_pasted_keys() {
        let other = |f: fn(&mut CustomProvider)| {
            let mut p = local_provider();
            f(&mut p);
            p
        };
        let same_id = [local_provider()];
        let cases: [(CustomProvider, &[CustomProvider], &[&str]); 5] = [
            (local_provider(), &[], &[]),
            (other(|p| p.api_key_env = "sk-ant-api03-abcdef".into()), &[], &["api_key_env"]),
            (other(|p| p.api_key_env = "1KEY".into()), &[], &["api_key_env"]),
            (other(|p| p.id = "My Provider".into()), &[], &["id"]),
            (local_provider(), &same_id, &["id"]),
        ];
        for (p, others, want) in cases {
            assert_eq!(provider_errors(&p, others).into_keys().collect::<Vec<_>>(), want, "{p:?}");
        }
    }

    #[test]
    fn baseline_and_errors_follow_saves_and_reverts() {
        let error = ValidationError { location: "modelRoles.default".into(), message: "unknown model".into() };
        let mut s = state();
        run(&mut s, vec![Local::ModelToggled(0, false)]);
        assert!(s.is_dirty());
        run(&mut s, vec![Local::ModelToggled(0, true)]);
        assert!(!s.is_dirty());
        for clear in [Local::Save, Local::Revert] {
            s.set_errors(vec![error.clone()]);
            run(&mut s, vec![Local::ProviderToggled(2, true)]);
            assert_eq!(s.errors().len(), 1, "edits keep the app's errors");
            run(&mut s, vec![clear]);
            assert!(s.errors().is_empty());
        }
        let Some(Event::Save(written)) = run(&mut s, vec![Local::ProviderToggled(2, true), Local::Save]) else { panic!("save") };
        assert!(s.is_dirty(), "a save waits for the app");
        s.set_errors(vec![error]);
        s.saved(written.clone());
        assert!(!s.is_dirty() && s.errors().is_empty());
        assert_eq!(s.draft_settings(), Ok(written));
    }

    #[test]
    fn the_save_button_writes_the_edited_settings() {
        let mut s = state();
        run(&mut s, vec![Local::ModelToggled(0, false)]);
        let events: Vec<Event> = click_label(s.view(), "Save").into_iter().filter_map(|m| s.update(m)).collect();
        assert_eq!(events, [saved(|s| s.models[0].enabled = false).unwrap()]);
    }
}
