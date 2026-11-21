//! Model settings screen: model roles with their fallback chains, enabled
//! models, provider order, per model overrides and custom providers.
//!
//! The screen edits omp's own `config.yml` and `models.yml` as plain data. The
//! app loads the files into [`ModelSettings`], hands them to [`State::new`],
//! writes what [`Event::Save`] carries and reports the outcome back through
//! [`State::saved`] or [`State::set_errors`].


use iced::widget::{checkbox, column, container, pick_list, row, scrollable, space, text};
use iced::{Element, Fill, Task};
use loa_ui_kit::icons::Icon;

use crate::style::{self, BODY, NOTE, SMALL};

use loa_models::settings::{
    AuthMode, CustomProvider, FieldValue, HarnessModel, ModelOverride, ModelSettings, ModelToggle,
    OverrideField, ProviderToggle, RoleChain, ValidationError, is_env_var_name, is_provider_id,
};


/// The effort pick list's entry for "no effort set".
const HARNESS_DEFAULT: &str = "default";

fn field_kind(value: &FieldValue) -> FieldKind {
    match value {
        FieldValue::Text(_) => FieldKind::Text,
        FieldValue::Int(_) => FieldKind::Int,
        FieldValue::Float(_) => FieldKind::Float,
        FieldValue::Bool(_) => FieldKind::Bool,
    }
}

fn field_raw(value: &FieldValue) -> String {
    match value {
        FieldValue::Text(value) => value.clone(),
        FieldValue::Int(value) => value.to_string(),
        FieldValue::Float(value) => value.to_string(),
        FieldValue::Bool(value) => value.to_string(),
    }
}

loa_util::str_enum! {
    /// The type an override value is edited as.
    pub FieldKind { Text => "Text", Int => "Integer", Float => "Number", Bool => "Yes/No" }
}

impl FieldKind {

    /// Parses a raw value typed by the user as this kind.
    fn parse_value(self, raw: &str) -> Result<FieldValue, String> {
        let trimmed = raw.trim();
        match self {
            Self::Text => Ok(FieldValue::Text(raw.to_owned())),
            Self::Int => trimmed
                .parse::<i64>()
                .map(FieldValue::Int)
                .map_err(|_| format!("\"{raw}\" is not a whole number")),
            Self::Float => match trimmed.parse::<f64>() {
                Ok(value) if value.is_finite() => Ok(FieldValue::Float(value)),
                _ => Err(format!("\"{raw}\" is not a number")),
            },
            Self::Bool => match trimmed {
                "true" => Ok(FieldValue::Bool(true)),
                "false" => Ok(FieldValue::Bool(false)),
                _ => Err(format!("\"{raw}\" is not true or false")),
            },
        }
    }
}




/// What the app must act on.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Write these settings to omp's config files, then call
    /// [`State::saved`] or [`State::set_errors`].
    Save(ModelSettings),
    /// The user discarded the unsaved changes.
    Reverted,
}

/// Moves the element at `from` to index `to`, shifting the ones between.
/// Returns false and leaves `items` unchanged when either index is out of
/// range.
pub fn move_item<T>(items: &mut Vec<T>, from: usize, to: usize) -> bool {
    if from >= items.len() || to >= items.len() {
        return false;
    }
    let item = items.remove(from);
    items.insert(to, item);
    true
}

/// Checks inexpensive form constraints that cannot be delegated to file validation.
pub fn validate_custom_provider(provider: &CustomProvider, others: &[CustomProvider]) -> Vec<String> {
    let mut messages = Vec::new();
    let id = provider.id.trim();
    if id.is_empty() {
        messages.push("Provider id is empty".to_owned());
    } else if !is_provider_id(id) || id.chars().any(|c| c.is_ascii_uppercase()) {
        messages.push("Provider id may only contain a-z, 0-9, -, _ and .".to_owned());
    } else if others.iter().any(|other| other.id.trim() == id) {
        messages.push(format!("Another custom provider already uses the id \"{id}\""));
    }

    let api_key_env = provider.api_key_env.trim();
    if !api_key_env.is_empty() && !is_env_var_name(api_key_env) {
        messages.push(
            "API key must be the name of an environment variable (A-Z, 0-9, _), not the key itself".to_owned(),
        );
    }
    messages
}


/// The editable form of [`ModelSettings`]: override values and custom provider
/// model lists stay raw text so the user can type freely.
#[derive(Clone, Debug, PartialEq)]
struct Draft {
    roles: Vec<RoleChain>,
    models: Vec<ModelToggle>,
    providers: Vec<ProviderToggle>,
    overrides: Vec<OverrideDraft>,
    custom_providers: Vec<CustomProviderDraft>,
    harnesses: Vec<HarnessModel>,
}

#[derive(Clone, Debug, PartialEq)]
struct OverrideDraft {
    selector: String,
    fields: Vec<FieldDraft>,
}

#[derive(Clone, Debug, PartialEq)]
struct FieldDraft {
    key: String,
    kind: FieldKind,
    raw: String,
}

#[derive(Clone, Debug, PartialEq)]
struct CustomProviderDraft {
    id: String,
    base_url: String,
    api: String,
    auth: AuthMode,
    api_key_env: String,
    /// Comma separated model ids.
    models: String,
}

impl CustomProviderDraft {
    fn to_provider(&self) -> CustomProvider {
        CustomProvider {
            id: self.id.trim().to_owned(),
            base_url: self.base_url.trim().to_owned(),
            api: self.api.trim().to_owned(),
            auth: self.auth,
            api_key_env: self.api_key_env.trim().to_owned(),
            models: self
                .models
                .split(',')
                .map(str::trim)
                .filter(|model| !model.is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }
}

impl Draft {
    fn new(settings: &ModelSettings) -> Self {
        Self {
            roles: settings.roles.clone(),
            models: settings.models.clone(),
            providers: settings.providers.clone(),
            overrides: settings
                .overrides
                .iter()
                .map(|o| OverrideDraft {
                    selector: o.selector.clone(),
                    fields: o
                        .fields
                        .iter()
                        .map(|f| FieldDraft { key: f.key.clone(), kind: field_kind(&f.value), raw: field_raw(&f.value) })
                        .collect(),
                })
                .collect(),
            custom_providers: settings
                .custom_providers
                .iter()
                .map(|p| CustomProviderDraft {
                    id: p.id.clone(),
                    base_url: p.base_url.clone(),
                    api: p.api.clone(),
                    auth: p.auth,
                    api_key_env: p.api_key_env.clone(),
                    models: p.models.join(", "),
                })
                .collect(),
            harnesses: settings.harnesses.clone(),
        }
    }

    fn custom_providers(&self) -> Vec<CustomProvider> {
        self.custom_providers.iter().map(CustomProviderDraft::to_provider).collect()
    }

    fn to_settings(&self) -> Result<ModelSettings, Vec<String>> {
        let mut errors = Vec::new();

        let mut roles = Vec::with_capacity(self.roles.len());
        for (index, role) in self.roles.iter().enumerate() {
            let name = role.role.trim();
            if name.is_empty() {
                errors.push(format!("Role {} has no name", index + 1));
            } else if self.roles[..index].iter().any(|r| r.role.trim() == name) {
                errors.push(format!("Role \"{name}\" is listed twice"));
            }
            roles.push(RoleChain { role: name.to_owned(), chain: role.chain.clone() });
        }

        let mut overrides = Vec::with_capacity(self.overrides.len());
        for o in &self.overrides {
            let mut fields = Vec::with_capacity(o.fields.len());
            for (index, field) in o.fields.iter().enumerate() {
                let key = field.key.trim();
                if key.is_empty() {
                    errors.push(format!("{}: field {} has no name", o.selector, index + 1));
                } else if o.fields[..index].iter().any(|f| f.key.trim() == key) {
                    errors.push(format!("{}: field \"{key}\" is set twice", o.selector));
                }
                match field.kind.parse_value(&field.raw) {
                    Ok(value) => fields.push(OverrideField { key: key.to_owned(), value }),
                    Err(message) => errors.push(format!("{}: {key}: {message}", o.selector)),
                }
            }
            overrides.push(ModelOverride { selector: o.selector.clone(), fields });
        }

        let custom_providers = self.custom_providers();
        for (index, provider) in custom_providers.iter().enumerate() {
            let others: Vec<CustomProvider> = custom_providers
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, p)| p.clone())
                .collect();
            let label = if provider.id.is_empty() {
                format!("Custom provider {}", index + 1)
            } else {
                provider.id.clone()
            };
            for message in validate_custom_provider(provider, &others) {
                errors.push(format!("{label}: {message}"));
            }
        }

        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(ModelSettings {
            roles,
            models: self.models.clone(),
            providers: self.providers.clone(),
            overrides,
            custom_providers,
            harnesses: self.harnesses.clone(),
        })
    }
}

/// An edit to one field of a custom provider.
#[derive(Clone, Debug)]
pub enum ProviderEdit {
    Id(String),
    BaseUrl(String),
    Api(String),
    Auth(AuthMode),
    ApiKeyEnv(String),
    /// Comma separated model ids.
    Models(String),
}

#[derive(Clone, Debug)]
pub enum Message {
    NewRoleInput(String),
    AddRole,
    RemoveRole(usize),
    RenameRole(usize, String),
    /// Text typed into a role's "add selector" input.
    ChainInput(usize, String),
    /// Adds the typed selector of a role's input to its chain.
    AddTypedChainEntry(usize),
    AddChainEntry(usize, String),
    RemoveChainEntry { role: usize, index: usize },
    MoveChainEntry { role: usize, from: usize, to: usize },
    ModelToggled(usize, bool),
    ProviderToggled(usize, bool),
    MoveProvider { from: usize, to: usize },
    NewOverrideInput(String),
    AddTypedOverride,
    AddOverride(String),
    RemoveOverride(usize),
    AddField(usize),
    FieldKeyChanged { model: usize, field: usize, key: String },
    FieldKindChanged { model: usize, field: usize, kind: FieldKind },
    FieldValueChanged { model: usize, field: usize, raw: String },
    RemoveField { model: usize, field: usize },
    AddCustomProvider,
    RemoveCustomProvider(usize),
    CustomProviderEdited(usize, ProviderEdit),
    HarnessModelChanged(usize, String),
    /// The effort chosen for a harness; [`HARNESS_DEFAULT`] clears it.
    HarnessEffortChanged(usize, String),
    Save,
    Revert,
}

/// The model settings screen.
#[derive(Clone, Debug)]
pub struct State {
    baseline: Draft,
    draft: Draft,
    known_models: Vec<String>,
    new_role: String,
    /// The "add selector" input of each role, parallel to `draft.roles`.
    chain_inputs: Vec<String>,
    new_override: String,
    /// Problems found in the draft by the last Save.
    local_errors: Vec<String>,
    /// Problems the app reported for the last write.
    app_errors: Vec<ValidationError>,
}

impl State {
    /// `known_models` are the selectors offered when adding chain entries and
    /// overrides.
    pub fn new(settings: ModelSettings, known_models: Vec<String>) -> Self {
        let draft = Draft::new(&settings);
        Self {
            chain_inputs: vec![String::new(); draft.roles.len()],
            baseline: draft.clone(),
            draft,
            known_models,
            new_role: String::new(),
            new_override: String::new(),
            local_errors: Vec::new(),
            app_errors: Vec::new(),
        }
    }

    /// The app wrote `settings`: they become the new baseline and draft.
    pub fn saved(&mut self, settings: ModelSettings) {
        self.baseline = Draft::new(&settings);
        self.reset_draft();
    }

    /// Shows the errors the app (or omp) reported for the last write.
    pub fn set_errors(&mut self, errors: Vec<ValidationError>) {
        self.app_errors = errors;
    }

    /// The errors last reported by the app.
    pub fn errors(&self) -> &[ValidationError] {
        &self.app_errors
    }

    /// Problems in the draft found by the last Save.
    pub fn local_errors(&self) -> &[String] {
        &self.local_errors
    }

    /// Whether the draft differs from the last loaded or saved settings.
    pub fn is_dirty(&self) -> bool {
        self.draft != self.baseline
    }

    /// The draft as settings, or the local problems that keep it from being
    /// saved.
    pub fn draft_settings(&self) -> Result<ModelSettings, Vec<String>> {
        self.draft.to_settings()
    }

    fn reset_draft(&mut self) {
        self.draft = self.baseline.clone();
        self.chain_inputs = vec![String::new(); self.draft.roles.len()];
        self.new_role.clear();
        self.new_override.clear();
        self.local_errors.clear();
        self.app_errors.clear();
    }

    fn add_chain_entry(&mut self, role: usize, selector: String) {
        let selector = selector.trim();
        if selector.is_empty() {
            return;
        }
        if let Some(role) = self.draft.roles.get_mut(role)
            && !role.chain.iter().any(|s| s == selector)
        {
            role.chain.push(selector.to_owned());
        }
    }

    fn add_override(&mut self, selector: String) {
        let selector = selector.trim();
        if selector.is_empty() || self.draft.overrides.iter().any(|o| o.selector == selector) {
            return;
        }
        self.draft.overrides.push(OverrideDraft { selector: selector.to_owned(), fields: Vec::new() });
    }

    fn field_mut(&mut self, model: usize, field: usize) -> Option<&mut FieldDraft> {
        self.draft.overrides.get_mut(model)?.fields.get_mut(field)
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        let mut event = None;
        match message {
            Message::NewRoleInput(value) => self.new_role = value,
            Message::AddRole => {
                let name = self.new_role.trim();
                if !name.is_empty() {
                    self.draft.roles.push(RoleChain { role: name.to_owned(), chain: Vec::new() });
                    self.chain_inputs.push(String::new());
                    self.new_role.clear();
                }
            }
            Message::RemoveRole(index) => {
                if index < self.draft.roles.len() {
                    self.draft.roles.remove(index);
                }
                if index < self.chain_inputs.len() {
                    self.chain_inputs.remove(index);
                }
            }
            Message::RenameRole(index, name) => {
                if let Some(role) = self.draft.roles.get_mut(index) {
                    role.role = name;
                }
            }
            Message::ChainInput(index, value) => {
                if let Some(input) = self.chain_inputs.get_mut(index) {
                    *input = value;
                }
            }
            Message::AddTypedChainEntry(index) => {
                if let Some(input) = self.chain_inputs.get_mut(index) {
                    let selector = std::mem::take(input);
                    self.add_chain_entry(index, selector);
                }
            }
            Message::AddChainEntry(index, selector) => self.add_chain_entry(index, selector),
            Message::RemoveChainEntry { role, index } => {
                if let Some(role) = self.draft.roles.get_mut(role)
                    && index < role.chain.len()
                {
                    role.chain.remove(index);
                }
            }
            Message::MoveChainEntry { role, from, to } => {
                if let Some(role) = self.draft.roles.get_mut(role) {
                    move_item(&mut role.chain, from, to);
                }
            }
            Message::ModelToggled(index, enabled) => {
                if let Some(model) = self.draft.models.get_mut(index) {
                    model.enabled = enabled;
                }
            }
            Message::ProviderToggled(index, enabled) => {
                if let Some(provider) = self.draft.providers.get_mut(index) {
                    provider.enabled = enabled;
                }
            }
            Message::MoveProvider { from, to } => {
                move_item(&mut self.draft.providers, from, to);
            }
            Message::NewOverrideInput(value) => self.new_override = value,
            Message::AddTypedOverride => {
                let selector = std::mem::take(&mut self.new_override);
                self.add_override(selector);
            }
            Message::AddOverride(selector) => self.add_override(selector),
            Message::RemoveOverride(index) => {
                if index < self.draft.overrides.len() {
                    self.draft.overrides.remove(index);
                }
            }
            Message::AddField(index) => {
                if let Some(o) = self.draft.overrides.get_mut(index) {
                    o.fields.push(FieldDraft { key: String::new(), kind: FieldKind::Text, raw: String::new() });
                }
            }
            Message::FieldKeyChanged { model, field, key } => {
                if let Some(field) = self.field_mut(model, field) {
                    field.key = key;
                }
            }
            Message::FieldKindChanged { model, field, kind } => {
                if let Some(field) = self.field_mut(model, field) {
                    if kind == FieldKind::Bool && field.kind != FieldKind::Bool {
                        // The checkbox edits only "true"/"false"; anything else reads as unchecked.
                        field.raw = (field.raw.trim() == "true").to_string();
                    }
                    field.kind = kind;
                }
            }
            Message::FieldValueChanged { model, field, raw } => {
                if let Some(field) = self.field_mut(model, field) {
                    field.raw = raw;
                }
            }
            Message::RemoveField { model, field } => {
                if let Some(o) = self.draft.overrides.get_mut(model)
                    && field < o.fields.len()
                {
                    o.fields.remove(field);
                }
            }
            Message::AddCustomProvider => self.draft.custom_providers.push(CustomProviderDraft {
                id: String::new(),
                base_url: String::new(),
                api: String::new(),
                auth: AuthMode::ApiKey,
                api_key_env: String::new(),
                models: String::new(),
            }),
            Message::RemoveCustomProvider(index) => {
                if index < self.draft.custom_providers.len() {
                    self.draft.custom_providers.remove(index);
                }
            }
            Message::CustomProviderEdited(index, edit) => {
                if let Some(provider) = self.draft.custom_providers.get_mut(index) {
                    match edit {
                        ProviderEdit::Id(value) => provider.id = value,
                        ProviderEdit::BaseUrl(value) => provider.base_url = value,
                        ProviderEdit::Api(value) => provider.api = value,
                        ProviderEdit::Auth(value) => provider.auth = value,
                        ProviderEdit::ApiKeyEnv(value) => provider.api_key_env = value,
                        ProviderEdit::Models(value) => provider.models = value,
                    }
                }
            }
            Message::HarnessModelChanged(index, model) => {
                if let Some(harness) = self.draft.harnesses.get_mut(index) {
                    harness.model = model;
                }
            }
            Message::HarnessEffortChanged(index, effort) => {
                if let Some(harness) = self.draft.harnesses.get_mut(index) {
                    harness.effort = if effort == HARNESS_DEFAULT { String::new() } else { effort };
                }
            }
            Message::Save => {
                self.app_errors.clear();
                match self.draft.to_settings() {
                    Ok(settings) => {
                        self.local_errors.clear();
                        event = Some(Event::Save(settings));
                    }
                    Err(errors) => self.local_errors = errors,
                }
            }
            Message::Revert => {
                self.reset_draft();
                event = Some(Event::Reverted);
            }
        }
        (Task::none(), event)
    }

    pub fn view(&self) -> Element<'_, Message> {
        let dirty = self.is_dirty();
        let mut header = row![text("Model settings").size(BODY), space::horizontal()].spacing(8).align_y(iced::Center);
        if dirty {
            header = header.push(text("Unsaved changes").size(SMALL).style(style::muted));
        }
        header = header
            .push(style::flat_button("Revert").on_press_maybe(dirty.then_some(Message::Revert)))
            .push(style::filled_button("Save").on_press_maybe(dirty.then_some(Message::Save)));

        let mut content = column![header].spacing(20).padding(12);

        if !self.app_errors.is_empty() {
            let list = self.app_errors.iter().fold(column![].spacing(4), |list, error| {
                list.push(text(format!("{}: {}", error.location, error.message)).size(NOTE))
            });
            content = content.push(container(list).style(style::error_box).padding(8).width(Fill));
        }
        if !self.local_errors.is_empty() {
            let list = self
                .local_errors
                .iter()
                .fold(column![].spacing(4), |list, error| list.push(text(error.as_str()).size(NOTE)));
            content = content.push(container(list).style(style::error_box).padding(8).width(Fill));
        }

        let content = content
            .push(section("Model roles", self.view_roles()))
            .push(section("Enabled models", self.view_models()))
            .push(section("Providers (order)", self.view_providers()))
            .push(section("Model overrides", self.view_overrides()))
            .push(section("Custom providers", self.view_custom_providers()))
            .push(section("Other harnesses", self.view_harnesses()));
        scrollable(content)
            .width(Fill)
            .height(Fill)
            .direction(style::thin_scrollbar())
            .style(style::scrollbar)
            .into()
    }

    fn view_roles(&self) -> Element<'_, Message> {
        let mut list = column![].spacing(12);
        for (i, role) in self.draft.roles.iter().enumerate() {
            let mut entries = column![
                row![
                    style::field("Role name", &role.role).on_input(move |name| Message::RenameRole(i, name)),
                    style::danger_button("Remove role").on_press(Message::RemoveRole(i)),
                ]
                .spacing(8)
                .align_y(iced::Center)
            ]
            .spacing(4);
            let len = role.chain.len();
            for (j, selector) in role.chain.iter().enumerate() {
                let label = if j == 0 { "Primary".to_owned() } else { format!("Fallback {j}") };
                entries = entries.push(
                    row![
                        text(label).size(SMALL).style(style::muted).width(80),
                        text(selector.as_str()).size(SMALL).font(iced::Font::MONOSPACE).width(Fill),
                        move_buttons(j, len, move |from, to| Message::MoveChainEntry { role: i, from, to }),
                        style::icon_button(Icon::X).on_press(Message::RemoveChainEntry { role: i, index: j }),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                );
            }
            let input = self.chain_inputs.get(i).map_or("", String::as_str);
            entries = entries.push(
                row![
                    pick(pick_list(self.known_models.as_slice(), None::<String>, move |selector| {
                        Message::AddChainEntry(i, selector)
                    })
                    .placeholder("Add model")),
                    style::field("or type provider/model[:thinking]", input)
                        .on_input(move |value| Message::ChainInput(i, value))
                        .on_submit(Message::AddTypedChainEntry(i)),
                    style::flat_button("Add").on_press(Message::AddTypedChainEntry(i)),
                ]
                .spacing(8)
                .align_y(iced::Center),
            );
            list = list.push(container(entries).style(style::card).padding(8).width(Fill));
        }
        list.push(
            row![
                style::field("New role name", &self.new_role).on_input(Message::NewRoleInput).on_submit(Message::AddRole),
                style::icon_label_button(Icon::Plus, "Add role").on_press(Message::AddRole),
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .into()
    }

    fn view_models(&self) -> Element<'_, Message> {
        if self.draft.models.is_empty() {
            return text("No models available").size(NOTE).style(style::muted).into();
        }
        self.draft
            .models
            .iter()
            .enumerate()
            .fold(column![].spacing(6), |list, (i, model)| {
                list.push(
                    checkbox(model.enabled)
                        .label(model.selector.as_str())
                        .size(14)
                        .text_size(NOTE)
                        .on_toggle(move |enabled| Message::ModelToggled(i, enabled)),
                )
            })
            .into()
    }

    fn view_providers(&self) -> Element<'_, Message> {
        if self.draft.providers.is_empty() {
            return text("No providers available").size(NOTE).style(style::muted).into();
        }
        let len = self.draft.providers.len();
        self.draft
            .providers
            .iter()
            .enumerate()
            .fold(column![].spacing(4), |list, (i, provider)| {
                let label = if provider.name.is_empty() {
                    provider.id.clone()
                } else {
                    format!("{} ({})", provider.name, provider.id)
                };
                list.push(
                    row![
                        checkbox(provider.enabled)
                            .label(label)
                            .size(14)
                            .text_size(NOTE)
                            .on_toggle(move |enabled| Message::ProviderToggled(i, enabled))
                            .width(Fill),
                        move_buttons(i, len, |from, to| Message::MoveProvider { from, to }),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                )
            })
            .into()
    }

    fn view_overrides(&self) -> Element<'_, Message> {
        let mut list = column![].spacing(12);
        for (i, o) in self.draft.overrides.iter().enumerate() {
            let mut fields = column![
                row![
                    text(o.selector.as_str()).size(SMALL).font(iced::Font::MONOSPACE).width(Fill),
                    style::icon_label_button(Icon::Plus, "Add field").on_press(Message::AddField(i)),
                    style::danger_button("Remove override").on_press(Message::RemoveOverride(i)),
                ]
                .spacing(4)
                .align_y(iced::Center)
            ]
            .spacing(4);
            for (j, field) in o.fields.iter().enumerate() {
                let value: Element<'_, Message> = if field.kind == FieldKind::Bool {
                    checkbox(field.raw.trim() == "true")
                        .label("true")
                        .size(14)
                        .text_size(NOTE)
                        .on_toggle(move |value| Message::FieldValueChanged { model: i, field: j, raw: value.to_string() })
                        .into()
                } else {
                    style::field("Value", &field.raw)
                        .on_input(move |raw| Message::FieldValueChanged { model: i, field: j, raw })
                        .into()
                };
                fields = fields.push(
                    row![
                        style::field("Field, e.g. contextWindow", &field.key)
                            .on_input(move |key| Message::FieldKeyChanged { model: i, field: j, key }),
                        pick(pick_list(FieldKind::ALL, Some(field.kind), move |kind| Message::FieldKindChanged {
                            model: i,
                            field: j,
                            kind
                        })),
                        value,
                        style::icon_button(Icon::X).on_press(Message::RemoveField { model: i, field: j }),
                    ]
                    .spacing(8)
                    .align_y(iced::Center),
                );
                if let Err(message) = field.kind.parse_value(&field.raw) {
                    fields = fields.push(text(message).size(SMALL).style(text::danger));
                }
            }
            list = list.push(container(fields).style(style::card).padding(8).width(Fill));
        }
        list.push(
            row![
                pick(pick_list(self.known_models.as_slice(), None::<String>, Message::AddOverride).placeholder("Override a model")),
                style::field("or type provider/model", &self.new_override)
                    .on_input(Message::NewOverrideInput)
                    .on_submit(Message::AddTypedOverride),
                style::flat_button("Add").on_press(Message::AddTypedOverride),
            ]
            .spacing(8)
            .align_y(iced::Center),
        )
        .into()
    }

    fn view_custom_providers(&self) -> Element<'_, Message> {
        let providers = self.draft.custom_providers();
        let mut list = column![].spacing(12);
        for (i, provider) in self.draft.custom_providers.iter().enumerate() {
            let edit = move |f: fn(String) -> ProviderEdit| move |value| Message::CustomProviderEdited(i, f(value));
            let mut form = column![
                labeled("Id", style::field("my-provider", &provider.id).on_input(edit(ProviderEdit::Id))),
                labeled(
                    "Base URL",
                    style::field("https://example.com/v1", &provider.base_url).on_input(edit(ProviderEdit::BaseUrl)),
                ),
                labeled("API", style::field("openai-completions", &provider.api).on_input(edit(ProviderEdit::Api))),
                labeled(
                    "Auth",
                    pick(pick_list(AuthMode::ALL, Some(provider.auth), move |auth| {
                        Message::CustomProviderEdited(i, ProviderEdit::Auth(auth))
                    })),
                ),
                labeled(
                    "API key variable",
                    style::field("MY_PROVIDER_API_KEY", &provider.api_key_env).on_input(edit(ProviderEdit::ApiKeyEnv)),
                ),
                labeled("Models", style::field("model-a, model-b", &provider.models).on_input(edit(ProviderEdit::Models))),
            ]
            .spacing(6);
            let others: Vec<CustomProvider> = providers
                .iter()
                .enumerate()
                .filter(|(other, _)| *other != i)
                .map(|(_, p)| p.clone())
                .collect();
            if let Some(current) = providers.get(i) {
                for message in validate_custom_provider(current, &others) {
                    form = form.push(text(message).size(SMALL).style(text::danger));
                }
            }
            form = form.push(style::danger_button("Remove provider").on_press(Message::RemoveCustomProvider(i)));
            list = list.push(container(form).style(style::card).padding(8).width(Fill));
        }
        list.push(style::icon_label_button(Icon::Plus, "Add custom provider").on_press(Message::AddCustomProvider))
            .into()
    }

    fn view_harnesses(&self) -> Element<'_, Message> {
        if self.draft.harnesses.is_empty() {
            return text("No other harness settings found").size(NOTE).style(style::muted).into();
        }
        let mut list = column![].spacing(12);
        for (i, harness) in self.draft.harnesses.iter().enumerate() {
            let efforts: Vec<String> =
                std::iter::once(HARNESS_DEFAULT.to_owned()).chain(harness.efforts.iter().cloned()).collect();
            let effort = if harness.effort.is_empty() { HARNESS_DEFAULT.to_owned() } else { harness.effort.clone() };
            let form = column![
                row![
                    text(&harness.name).size(BODY),
                    text(&harness.config_path).size(SMALL).style(style::muted).font(iced::Font::MONOSPACE),
                ]
                .spacing(8)
                .align_y(iced::Center),
                labeled(
                    "Model",
                    style::field("harness default", &harness.model)
                        .on_input(move |model| Message::HarnessModelChanged(i, model)),
                ),
                labeled(
                    "Effort",
                    pick(pick_list(efforts, Some(effort), move |effort| Message::HarnessEffortChanged(i, effort))),
                ),
            ]
            .spacing(6);
            list = list.push(container(form).style(style::card).padding(8).width(Fill));
        }
        list.into()
    }
}

fn section<'a>(title: &'a str, body: Element<'a, Message>) -> Element<'a, Message> {
    column![style::section_header(title), style::divider(), body].spacing(8).into()
}

fn labeled<'a>(label: &'a str, field: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![text(label).size(SMALL).style(style::muted).width(110), field.into()]
        .spacing(8)
        .align_y(iced::Center)
        .into()
}

/// A pick list styled like the other raised fields.
fn pick<'a, T, L, V>(list: pick_list::PickList<'a, T, L, V, Message>) -> pick_list::PickList<'a, T, L, V, Message>
where
    T: ToString + PartialEq + Clone,
    L: std::borrow::Borrow<[T]> + 'a,
    V: std::borrow::Borrow<T> + 'a,
{
    list.text_size(NOTE).padding([5.0, 8.0]).style(style::pick)
}

/// Up and down buttons for the item at `index` of a list of `len`, disabled at
/// the ends. `on_move(from, to)` builds the message.
fn move_buttons<'a>(index: usize, len: usize, on_move: impl Fn(usize, usize) -> Message) -> Element<'a, Message> {
    let up = index.checked_sub(1).map(|to| on_move(index, to));
    let down = (index + 1 < len).then(|| on_move(index, index + 1));
    let arrow = |icon: Icon, message: Option<Message>| -> Element<'a, Message> {
        match message {
            Some(message) => style::icon_button(icon).on_press(message).into(),
            None => space().width(20).height(20).into(),
        }
    };
    row![arrow(Icon::ChevronUp, up), arrow(Icon::ChevronDown, down)].spacing(2).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> ModelSettings {
        ModelSettings {
            roles: vec![RoleChain {
                role: "default".into(),
                chain: vec![
                    "anthropic/claude-sonnet-4-5:high".into(),
                    "openai/gpt-5".into(),
                    "google/gemini-2.5-pro".into(),
                ],
            }],
            models: vec![ModelToggle { selector: "openai/gpt-5".into(), enabled: true }],
            providers: vec![
                ProviderToggle { id: "anthropic".into(), name: "Anthropic".into(), enabled: true },
                ProviderToggle { id: "openai".into(), name: "OpenAI".into(), enabled: true },
                ProviderToggle { id: "google".into(), name: "Google".into(), enabled: false },
            ],
            overrides: vec![ModelOverride {
                selector: "openai/gpt-5".into(),
                fields: vec![OverrideField { key: "contextWindow".into(), value: FieldValue::Int(400_000) }],
            }],
            custom_providers: Vec::new(),
            harnesses: vec![HarnessModel {
                harness: "codex".into(),
                name: "Codex".into(),
                config_path: "~/.codex/config.toml".into(),
                model: String::new(),
                effort: String::new(),
                efforts: vec!["low".into(), "high".into()],
            }],
        }
    }

    fn state() -> State {
        State::new(settings(), vec!["openai/gpt-5".into(), "anthropic/claude-haiku-4-5".into()])
    }

    /// Sends a message and returns the event; the task is never needed here.
    fn send(state: &mut State, message: Message) -> Option<Event> {
        let (_task, event) = state.update(message);
        event
    }

    fn saved_settings(state: &mut State) -> ModelSettings {
        match state.update(Message::Save) {
            (_task, Some(Event::Save(settings))) => settings,
            (_task, other) => panic!("expected Save, got {other:?}; local errors {:?}", state.local_errors()),
        }
    }

    fn full_provider() -> CustomProvider {
        CustomProvider {
            id: "local-llm".into(),
            base_url: "http://localhost:8080/v1".into(),
            api: "openai-completions".into(),
            auth: AuthMode::ApiKey,
            api_key_env: "LOCAL_LLM_KEY".into(),
            models: vec!["qwen3".into()],
        }
    }

    #[test]
    fn move_item_moves_within_range_and_rejects_out_of_range() {
        let mut items = vec!['a', 'b', 'c'];
        assert!(move_item(&mut items, 2, 0));
        assert_eq!(items, ['c', 'a', 'b']);
        assert!(move_item(&mut items, 0, 2));
        assert_eq!(items, ['a', 'b', 'c']);
        assert!(!move_item(&mut items, 3, 0));
        assert!(!move_item(&mut items, 0, 3));
        assert_eq!(items, ['a', 'b', 'c']);
        assert!(!move_item(&mut Vec::<char>::new(), 0, 0));
    }

    #[test]
    fn moving_a_fallback_to_the_top_makes_it_primary_in_save() {
        let mut state = state();
        send(&mut state, Message::MoveChainEntry { role: 0, from: 2, to: 0 });
        let saved = saved_settings(&mut state);
        assert_eq!(
            saved.roles[0].chain,
            ["google/gemini-2.5-pro", "anthropic/claude-sonnet-4-5:high", "openai/gpt-5"]
        );
        // Save leaves the baseline alone until the app confirms the write.
        assert!(state.is_dirty());
    }

    #[test]
    fn added_chain_entries_append_as_fallbacks() {
        let mut state = state();
        send(&mut state, Message::AddChainEntry(0, "anthropic/claude-haiku-4-5".into()));
        send(&mut state, Message::ChainInput(0, " local/qwen3 ".into()));
        send(&mut state, Message::AddTypedChainEntry(0));
        send(&mut state, Message::RemoveChainEntry { role: 0, index: 1 });
        let saved = saved_settings(&mut state);
        assert_eq!(
            saved.roles[0].chain,
            ["anthropic/claude-sonnet-4-5:high", "google/gemini-2.5-pro", "anthropic/claude-haiku-4-5", "local/qwen3"]
        );
    }

    #[test]
    fn provider_order_moves_are_saved() {
        let mut state = state();
        send(&mut state, Message::MoveProvider { from: 2, to: 1 });
        send(&mut state, Message::MoveProvider { from: 0, to: 1 });
        let ids: Vec<String> = saved_settings(&mut state).providers.into_iter().map(|p| p.id).collect();
        assert_eq!(ids, ["google", "anthropic", "openai"]);
    }

    #[test]
    fn dirty_tracks_the_draft_against_the_baseline() {
        let mut state = state();
        assert!(!state.is_dirty());
        send(&mut state, Message::ModelToggled(0, false));
        assert!(state.is_dirty());
        send(&mut state, Message::ModelToggled(0, true));
        assert!(!state.is_dirty());

        send(&mut state, Message::RenameRole(0, "smol".into()));
        assert!(state.is_dirty());
        let (_task, event) = state.update(Message::Revert);
        assert_eq!(event, Some(Event::Reverted));
        assert!(!state.is_dirty());
        assert_eq!(state.draft_settings(), Ok(settings()));
    }

    #[test]
    fn save_refuses_an_unparsable_int_until_fixed() {
        let mut state = state();
        send(&mut state, Message::FieldValueChanged { model: 0, field: 0, raw: "12x".into() });
        let (_task, event) = state.update(Message::Save);
        assert_eq!(event, None);
        assert_eq!(state.local_errors().len(), 1);
        assert!(state.local_errors()[0].contains("contextWindow"), "{:?}", state.local_errors());

        send(&mut state, Message::FieldValueChanged { model: 0, field: 0, raw: "128000".into() });
        let saved = saved_settings(&mut state);
        assert_eq!(saved.overrides[0].fields[0].value, FieldValue::Int(128_000));
        assert!(state.local_errors().is_empty());
    }

    #[test]
    fn float_and_bool_fields_parse_to_typed_values() {
        let mut state = state();
        send(&mut state, Message::AddOverride("anthropic/claude-haiku-4-5".into()));
        send(&mut state, Message::AddField(1));
        send(&mut state, Message::FieldKeyChanged { model: 1, field: 0, key: "temperature".into() });
        send(&mut state, Message::FieldKindChanged { model: 1, field: 0, kind: FieldKind::Float });
        send(&mut state, Message::FieldValueChanged { model: 1, field: 0, raw: "abc".into() });
        assert!(state.draft_settings().is_err());
        send(&mut state, Message::FieldValueChanged { model: 1, field: 0, raw: "0.5".into() });

        send(&mut state, Message::AddField(1));
        send(&mut state, Message::FieldKeyChanged { model: 1, field: 1, key: "reasoning".into() });
        send(&mut state, Message::FieldKindChanged { model: 1, field: 1, kind: FieldKind::Bool });
        send(&mut state, Message::FieldValueChanged { model: 1, field: 1, raw: true.to_string() });

        let saved = saved_settings(&mut state);
        assert_eq!(
            saved.overrides[1],
            ModelOverride {
                selector: "anthropic/claude-haiku-4-5".into(),
                fields: vec![
                    OverrideField { key: "temperature".into(), value: FieldValue::Float(0.5) },
                    OverrideField { key: "reasoning".into(), value: FieldValue::Bool(true) },
                ],
            }
        );
    }

    #[test]
    fn empty_and_duplicate_names_block_save() {
        let mut state = state();
        send(&mut state, Message::AddField(0));
        send(&mut state, Message::NewRoleInput("plan".into()));
        send(&mut state, Message::AddRole);
        send(&mut state, Message::RenameRole(1, "default".into()));
        let (_task, event) = state.update(Message::Save);
        assert_eq!(event, None);
        assert_eq!(state.local_errors().len(), 2, "{:?}", state.local_errors());
    }


    #[test]
    fn custom_provider_rejects_a_pasted_secret_key() {
        let provider = CustomProvider { api_key_env: "sk-ant-api03-abcdef".into(), ..full_provider() };
        let messages = validate_custom_provider(&provider, &[]);
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("environment variable"));
        let digit_first = CustomProvider { api_key_env: "1KEY".into(), ..full_provider() };
        assert_eq!(validate_custom_provider(&digit_first, &[]).len(), 1);
    }


    #[test]
    fn custom_provider_rejects_duplicate_and_bad_id() {
        let duplicate = validate_custom_provider(&full_provider(), &[full_provider()]);
        assert_eq!(duplicate.len(), 1, "{duplicate:?}");
        assert!(duplicate[0].contains("already uses"));

        let bad_id = CustomProvider { id: "My Provider".into(), ..full_provider() };
        assert_eq!(validate_custom_provider(&bad_id, &[]).len(), 1);
    }

    #[test]
    fn custom_provider_edits_are_saved_and_validated() {
        let mut state = state();
        send(&mut state, Message::AddCustomProvider);
        let (_task, event) = state.update(Message::Save);
        assert_eq!(event, None);
        assert!(!state.local_errors().is_empty());

        let edits = [
            ProviderEdit::Id("local-llm".into()),
            ProviderEdit::BaseUrl("http://localhost:8080/v1".into()),
            ProviderEdit::Api("openai-completions".into()),
            ProviderEdit::ApiKeyEnv("LOCAL_LLM_KEY".into()),
            ProviderEdit::Models(" qwen3 ,, llama-4 ".into()),
        ];
        for edit in edits {
            send(&mut state, Message::CustomProviderEdited(0, edit));
        }
        let saved = saved_settings(&mut state);
        assert_eq!(
            saved.custom_providers,
            [CustomProvider { models: vec!["qwen3".into(), "llama-4".into()], ..full_provider() }]
        );
    }

    #[test]
    fn app_errors_stay_until_save_revert_or_saved() {
        let error = ValidationError { location: "modelRoles.default".into(), message: "unknown model".into() };
        let mut state = state();

        state.set_errors(vec![error.clone()]);
        send(&mut state, Message::ModelToggled(0, false));
        assert_eq!(state.errors(), std::slice::from_ref(&error));
        send(&mut state, Message::Save);
        assert!(state.errors().is_empty());

        state.set_errors(vec![error.clone()]);
        send(&mut state, Message::Revert);
        assert!(state.errors().is_empty());

        state.set_errors(vec![error.clone()]);
        state.saved(settings());
        assert!(state.errors().is_empty());
    }

    #[test]
    fn saved_makes_the_written_settings_the_new_baseline() {
        let mut state = state();
        send(&mut state, Message::ProviderToggled(2, true));
        let written = saved_settings(&mut state);
        state.saved(written.clone());
        assert!(!state.is_dirty());
        assert_eq!(state.draft_settings(), Ok(written));

        // Toggling back now differs from the new baseline.
        send(&mut state, Message::ProviderToggled(2, false));
        assert!(state.is_dirty());
    }
}
