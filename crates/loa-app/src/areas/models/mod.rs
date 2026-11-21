//! Models (M1): the sign in, model and model settings screens, and the
//! composer's model and effort pickers.
//!
//! The screens come from loa-ui-settings and talk to omp through its
//! [`Backend`] (a dedicated `omp --mode rpc` process, omp's config files).
//! Model and effort changes go to the active session's harness; a session
//! whose harness isn't running gets them in its config when it starts or
//! resumes.

use loa_util::LockExt;
use std::collections::HashMap;
use std::sync::Mutex;

use iced::futures::SinkExt;
use iced::widget::container;
use iced::{Element, Fill, Subscription, Task};
use loa_contracts::harness::{AdapterError, HarnessModel, ModelSelection, SessionConfig};
use loa_bus::SessionId;
use loa_core::{Dom, Op, PatchSink, attr};
use loa_harness::omp::account::OmpLaunch;
use loa_ui_session::composer as composer;
use loa_models::service::{Backend, SettingsLoad};
use loa_models::settings::{ModelSettings, ValidationError};
use loa_ui_kit::widgets::{Msg, Screen};
use loa_ui_settings::models::{self as screen, Catalog, ModelInfo, ModelRef, Page, RoleAssignment, SessionModel, picker, role_draft, settings, signin};
use loa_ui_settings::models::signin::{LoginUpdate, ProviderEntry, ProviderKey};

use crate::area::{Area, AreaCommand, AreaMessage, Ctx, Session, SessionEvent, Target};

#[cfg(test)]
mod tests;

/// The composer lists every model of a harness offering at most this many;
/// otherwise the current model and the ones serving a role. "All models…"
/// opens the full picker.
const COMPOSER_MODELS: usize = 12;

/// Rows under the composer's model choices, by [`composer::PickerAction`] id.
const ACTIONS: [(&str, &str, Page); 3] = [
    ("all", "All models…", Page::Model),
    ("settings", "Model settings…", Page::Settings),
    ("sign-in", "Sign in…", Page::SignIn),
];

#[derive(Clone, Debug)]
pub enum Message {
    /// The user picked a model in the composer.
    Model(composer::ModelOption),
    /// The user picked an effort in the composer.
    Effort(String),
    /// The user pressed one of the composer's model actions.
    Action(String),
    /// Shows a page of the models screen, loading it.
    Show(Page),
    Close,
    /// The models a harness lists for its sessions.
    Listed { harness: String, result: Result<Vec<HarnessModel>, String> },
    /// omp's models and role assignments.
    Catalog(Result<Catalog, String>),
    ModelSet { session: SessionId, model: String, role: Option<String>, result: Result<(), Failure> },
    EffortSet { session: SessionId, harness: String, effort: String, result: Result<(), Failure> },
    Providers(Vec<ProviderEntry>),
    SignIn(signin::Message),
    Login(ProviderKey, LoginUpdate),
    Picker(picker::Message),
    RoleAssigned(Result<(), String>),
    SettingsLoaded(Result<SettingsLoad, String>),
    Settings(settings::Message),
    Saved(Result<ModelSettings, Vec<ValidationError>>),
}

/// Why a harness call failed.
#[derive(Clone, Debug)]
pub enum Failure {
    /// The harness can't do it during a session.
    Unsupported(String),
    Error(String),
}

impl From<AdapterError> for Failure {
    fn from(error: AdapterError) -> Self {
        match error {
            AdapterError::Unsupported { .. } => Failure::Unsupported(error.to_string()),
            error => Failure::Error(error.to_string()),
        }
    }
}

/// A page's content while it is read.
enum Load<T> {
    Loading,
    Failed(String),
    Ready(T),
}

/// The open model picker and how its picks reach the harness.
struct Picker {
    state: picker::State,
    /// The harness model id of each row's model.
    ids: HashMap<ModelRef, String>,
}

/// A model or effort chosen while the session's harness wasn't running,
/// passed in the config when it starts or resumes.
#[derive(Clone, Debug, Default)]
struct Pending {
    harness: String,
    /// Harness model id and the role it was picked through.
    model: Option<(String, Option<String>)>,
    effort: Option<String>,
}

pub struct State {
    pub(crate) backend: Backend,
    page: Page,
    signin: Load<signin::State>,
    /// The harness whose models the picker shows, and the picker.
    picker: (String, Load<Picker>),
    settings: Load<settings::State>,
    /// The models each harness listed, by harness id. An empty list stands
    /// for a harness that can't list them.
    listed: HashMap<String, Vec<HarnessModel>>,
    /// omp's role assignments; `None` until read.
    roles: Option<Vec<RoleAssignment>>,
    /// Keyed by session id; `None` is the next new session.
    pending: Mutex<HashMap<Option<String>, Pending>>,
}

/// Effort levels offered for omp before a session runs and lists its own.
const PRE_SESSION_EFFORTS: [&str; 4] = ["low", "medium", "high", "xhigh"];

impl State {
    pub(crate) fn with_backend(backend: Backend) -> Self {
        Self {
            backend,
            page: Page::Model,
            signin: Load::Loading,
            picker: (String::new(), Load::Loading),
            settings: Load::Loading,
            listed: HashMap::new(),
            roles: None,
            pending: Mutex::default(),
        }
    }

    fn read_providers(&self) -> Task<Message> {
        let backend = self.backend.clone();
        Task::perform(async move { backend.providers().await }, Message::Providers)
    }

    /// Shows `page` and reads what it shows.
    fn show(&mut self, page: Page, ctx: &mut Ctx) -> Task<Message> {
        self.page = page;
        ctx.show_screen(Self::ID);
        match page {
            Page::SignIn => {
                if !matches!(self.signin, Load::Ready(_)) {
                    self.signin = Load::Loading;
                }
                self.read_providers()
            }
            Page::Model => self.load_picker(ctx),
            Page::Settings => {
                // Unsaved edits stay when the page is shown again.
                if let Load::Ready(state) = &self.settings
                    && state.is_dirty()
                {
                    return Task::none();
                }
                self.settings = Load::Loading;
                let backend = self.backend.clone();
                Task::perform(async move { backend.load_settings().await }, Message::SettingsLoaded)
            }
        }
    }

    /// The harness model changes go to: the active session's, else the one
    /// new sessions start on.
    fn harness_id(ctx: &Ctx) -> &str {
        ctx.sessions.active().and_then(Session::harness_id).or(ctx.sessions.harness.as_deref()).unwrap_or_else(|| ctx.services.harnesses.roles_id())
    }

    fn load_picker(&mut self, ctx: &Ctx) -> Task<Message> {
        let harness = Self::harness_id(ctx).to_owned();
        self.picker = (harness.clone(), Load::Loading);
        if roles(ctx, &harness) {
            let backend = self.backend.clone();
            return Task::perform(async move { backend.catalog().await }, Message::Catalog);
        }
        if self.listed.get(&harness).is_some_and(|models| !models.is_empty()) {
            self.open_picker(ctx);
            return Task::none();
        }
        match ctx.sessions.active().and_then(Session::harness).cloned() {
            Some(session) => Task::perform(async move { session.models().await.map_err(|error| error.to_string()) }, {
                move |result| Message::Listed { harness: harness.clone(), result }
            }),
            None => {
                let label = ctx.services.harnesses.label(&harness);
                self.picker.1 = Load::Failed(format!("{label} lists its models once the session runs"));
                Task::none()
            }
        }
    }

    /// Opens the picker over the harness's models once they are known.
    fn open_picker(&mut self, ctx: &Ctx) {
        let (harness, _) = &self.picker;
        let (catalog, ids) = if roles(ctx, harness) {
            let Some(models) = self.listed.get(ctx.services.harnesses.roles_id()) else { return };
            let catalog = Catalog {
                models: models.iter().map(|model| model_info(model, &ModelRef::parse(&model.id).unwrap_or_else(|| model_ref(model)))).collect(),
                roles: self.roles.clone().unwrap_or_default(),
            };
            let ids = catalog.models.iter().map(|info| (info.model.clone(), info.model.selector())).collect();
            (catalog, ids)
        } else {
            let Some(models) = self.listed.get(harness).filter(|models| !models.is_empty()) else { return };
            let ids: HashMap<ModelRef, String> = models.iter().map(|model| (model_ref(model), model.id.clone())).collect();
            let catalog = Catalog { models: models.iter().map(|model| model_info(model, &model_ref(model))).collect(), roles: Vec::new() };
            (catalog, ids)
        };
        let current = self.current(ctx).model.and_then(|id| {
            ids.iter().find(|(_, harness_id)| **harness_id == id).map(|(model, _)| model.clone()).or_else(|| ModelRef::parse(&id))
        });
        let (state, _focus) = picker::State::open(catalog, picker::Target::Session, current);
        self.picker.1 = Load::Ready(Picker { state, ids });
    }

    /// The active session's model and role, with a pending pick on top.
    fn current(&self, ctx: &Ctx) -> SessionModel {
        let Some(session) = ctx.sessions.active() else {
            if let Some((model, role)) = self.pending.locked().get(&None).and_then(|pending| pending.model.clone()) {
                return SessionModel { model: Some(model), role };
            }
            // A new omp session runs on the default role's model.
            let default = roles(ctx, Self::harness_id(ctx))
                .then(|| self.roles.as_deref().unwrap_or_default().iter().find(|role| role.role == "default"))
                .flatten();
            return default.map_or_else(SessionModel::default, |role| SessionModel { model: Some(role.model.selector()), role: None });
        };
        let mut current = SessionModel::from_dom(session.dom()).map(|(_, model)| model).unwrap_or_default();
        if let Some((model, role)) = self.pending.locked().get(&Some(session.id().to_string())).and_then(|pending| pending.model.clone()) {
            current = SessionModel { model: Some(model), role };
        }
        current
    }

    /// Switches the active session to `model` (a harness model id), picked
    /// through `role` when that is set.
    fn choose(&mut self, model: String, role: Option<String>, ctx: &mut Ctx) -> Task<Message> {
        let harness = Self::harness_id(ctx).to_owned();
        let Some(session) = ctx.sessions.active() else {
            ctx.notify(format!("The next session starts on {model}"));
            self.pending.locked().insert(None, Pending { harness, model: Some((model, role)), effort: None });
            return Task::none();
        };
        let id = session.id().clone();
        let Some(running) = session.harness().cloned() else {
            ctx.notify(format!("This session continues on {model} when it resumes"));
            self.pending.locked().entry(Some(id.to_string())).or_insert_with(|| Pending { harness, ..Pending::default() }).model = Some((model, role));
            self.sync(ctx);
            return Task::none();
        };
        let selection = ModelSelection { model: model.clone(), role: None };
        Task::perform(async move { running.set_model(selection).await.map_err(Failure::from) }, move |result| {
            Message::ModelSet { session: id.clone(), model: model.clone(), role: role.clone(), result }
        })
    }

    fn set_effort(&mut self, effort: String, ctx: &mut Ctx) -> Task<Message> {
        let harness = Self::harness_id(ctx).to_owned();
        let Some(session) = ctx.sessions.active() else {
            self.pending.locked().entry(None).or_insert_with(|| Pending { harness, ..Pending::default() }).effort = Some(effort);
            self.sync(ctx);
            return Task::none();
        };
        let id = session.id().clone();
        let Some(running) = session.harness().cloned() else {
            ctx.notify(format!("This session continues with {effort} effort when it resumes"));
            self.pending.locked().entry(Some(id.to_string())).or_insert_with(|| Pending { harness, ..Pending::default() }).effort = Some(effort);
            self.sync(ctx);
            return Task::none();
        };
        let level = effort.clone();
        Task::perform(async move { running.set_effort(level).await.map_err(Failure::from) }, move |result| Message::EffortSet {
            session: id.clone(),
            harness: harness.clone(),
            effort: effort.clone(),
            result,
        })
    }

    /// Asks the active session's running harness for its models, once per
    /// harness.
    fn list_models(&self, ctx: &Ctx) -> Task<Message> {
        let Some(session) = ctx.sessions.active() else { return Task::none() };
        let (Some(harness), Some(running)) = (session.harness_id(), session.harness().cloned()) else { return Task::none() };
        if self.listed.contains_key(harness) {
            return Task::none();
        }
        let harness = harness.to_owned();
        Task::perform(async move { running.models().await.map_err(|error| error.to_string()) }, move |result| Message::Listed {
            harness: harness.clone(),
            result,
        })
    }

    /// Reads omp's catalog so the composer offers models before any session
    /// runs. The app calls it once at startup.
    pub(crate) fn read_catalog(&self) -> Task<Message> {
        self.read_roles()
    }

    fn read_roles(&self) -> Task<Message> {
        let backend = self.backend.clone();
        Task::perform(async move { backend.catalog().await }, Message::Catalog)
    }

    /// Shows the active session's model and effort in the composer pickers,
    /// with the harness's models to switch to.
    fn sync(&self, ctx: &mut Ctx) {
        let current = self.current(ctx);
        let session = ctx.sessions.active();
        let harness = Self::harness_id(ctx);
        let pending_effort = self.pending.locked().get(&session.map(|session| session.id().to_string())).and_then(|p| p.effort.clone());
        let effort = pending_effort.or_else(|| session.and_then(|session| session.dom().root()?.str_attr(attr::EFFORT).map(str::to_owned)));
        let roles = if roles(ctx, harness) { self.roles.as_deref().unwrap_or_default() } else { &[] };
        let role_of = |id: &str| roles.iter().find(|role| role.model.selector() == id).map(|role| role.role.clone());
        let listed = self.listed.get(harness).map_or(&[][..], Vec::as_slice);
        let shown = |model: &&HarnessModel| {
            listed.len() <= COMPOSER_MODELS || current.model.as_deref() == Some(model.id.as_str()) || role_of(&model.id).is_some()
        };
        let mut options: Vec<composer::ModelOption> = listed
            .iter()
            .filter(shown)
            .map(|model| composer::ModelOption {
                id: model.id.clone(),
                name: model.name.clone(),
                provider: model.provider.clone(),
                role: role_of(&model.id),
            })
            .collect();
        if let Some(id) = &current.model {
            match options.iter_mut().find(|option| &option.id == id) {
                Some(option) => option.role = current.role.clone().or(option.role.take()),
                None => {
                    let (provider, name) = id.split_once('/').unwrap_or(("", id));
                    options.push(composer::ModelOption { id: id.clone(), name: name.to_owned(), provider: provider.to_owned(), role: current.role.clone() });
                }
            }
        }
        let mut efforts = current.model.as_ref().and_then(|id| listed.iter().find(|model| &model.id == id)).map(|model| model.efforts.clone()).unwrap_or_default();
        // omp's catalog doesn't say which levels a model takes; before a
        // session runs, offer omp's levels.
        if efforts.is_empty() && session.is_none() && self::roles(ctx, harness) && current.model.is_some() {
            efforts = PRE_SESSION_EFFORTS.iter().map(|level| (*level).to_owned()).collect();
        }
        if let Some(effort) = &effort
            && !efforts.contains(effort)
        {
            efforts.push(effort.clone());
        }
        let actions = if session.is_some() {
            ACTIONS.iter().map(|(id, label, _)| composer::PickerAction { id: (*id).to_owned(), label: (*label).to_owned() }).collect()
        } else {
            Vec::new()
        };
        ctx.composer.set_models(options, current.model.as_deref());
        ctx.composer.set_efforts(efforts, effort);
        ctx.composer.set_model_actions(actions);
    }

    fn signin_event(&mut self, event: signin::Event, ctx: &mut Ctx) -> Task<Message> {
        match event {
            signin::Event::SignIn(key) => {
                let backend = self.backend.clone();
                let login_key = key.clone();
                let updates = iced::stream::channel(16, async move |mut out| {
                    let mut updates = backend.login(login_key);
                    while let Some(update) = updates.recv().await {
                        if out.send(update).await.is_err() { break; }
                    }
                });
                Task::run(updates, move |update| Message::Login(key.clone(), update))
            }
            signin::Event::OpenUrl(url) => {
                if let Err(error) = self.backend.open_url(&url) {
                    ctx.notify(format!("The browser did not open ({error}); copy the URL instead"));
                }
                Task::none()
            }
            signin::Event::CopyUrl(url) => iced::clipboard::write(url),
            signin::Event::Cancel(key) => {
                self.backend.cancel(&key);
                Task::none()
            }
            signin::Event::SubmitInput { key, text } => {
                self.backend.submit_input(&key, text);
                Task::none()
            }
        }
    }

    fn picker_event(&mut self, event: picker::Event, ctx: &mut Ctx) -> Task<Message> {
        match event {
            picker::Event::SetSessionModel(change) => {
                let Load::Ready(picker) = &self.picker.1 else { return Task::none() };
                let model = picker.ids.get(&change.model).cloned().unwrap_or_else(|| change.model.selector());
                ctx.close_screen();
                self.choose(model, change.role, ctx)
            }
            picker::Event::SetRoleModel { role, model } => {
                let backend = self.backend.clone();
                Task::perform(async move { backend.assign_role(role, model).await }, Message::RoleAssigned)
            }
            picker::Event::Close => {
                ctx.close_screen();
                Task::none()
            }
        }
    }

    /// omp's models or settings changed: lists are read again.
    fn refresh_models(&mut self, ctx: &mut Ctx) -> Task<Message> {
        self.listed.clear();
        self.roles = None;
        self.sync(ctx);
        let listed = self.list_models(ctx);
        if roles(ctx, Self::harness_id(ctx)) { Task::batch([listed, self.read_roles()]) } else { listed }
    }
}

/// Whether `harness` has model roles, read through the omp models backend.
fn roles(ctx: &Ctx, harness: &str) -> bool {
    ctx.services.harnesses.traits(harness).has_roles
}

/// The picker's name for a harness model: its `provider/id` selector when
/// the id is one, else the provider and the bare id.
fn model_ref(model: &HarnessModel) -> ModelRef {
    ModelRef::parse(&model.id).filter(|parsed| parsed.provider == model.provider).unwrap_or_else(|| ModelRef::new(&model.provider, &model.id))
}

fn model_info(model: &HarnessModel, reference: &ModelRef) -> ModelInfo {
    ModelInfo { model: reference.clone(), name: model.name.clone(), provider_name: String::new() }
}

/// Records on `session` the role its model was picked through, or that it
/// no longer follows one. The harness records the model itself.
fn record_role(session: &Session, role: Option<String>) -> Result<(), String> {
    let writer = session.writer();
    match writer.with_dom(|dom: &Dom| role_draft(dom, role.as_deref())) {
        Some(draft) => writer.commit(draft).map(drop).map_err(|error| error.to_string()),
        None => Ok(()),
    }
}

impl State {
    /// One page's body without the screen's page switch and close button,
    /// for Settings and Configs, which draw their own navigation.
    pub(crate) fn page_body(&self, page: Page) -> Element<'_, Message> {
        match page {
            Page::SignIn => match &self.signin {
                Load::Ready(state) => state.view().map(Message::SignIn),
                Load::Loading => screen::status("Reading sign in status…"),
                Load::Failed(error) => screen::status(error.as_str()),
            },
            Page::Model => match &self.picker.1 {
                Load::Ready(picker) => {
                    container(picker.state.view().map(Message::Picker)).center_x(Fill).padding(loa_ui_kit::theme::pad_y(loa_ui_kit::theme::SpaceRole::Xl)).into()
                }
                Load::Loading => screen::status("Reading models…"),
                Load::Failed(error) => screen::status(error.as_str()),
            },
            Page::Settings => match &self.settings {
                Load::Ready(state) => state.view().map(Message::Settings),
                Load::Loading => screen::status("Reading omp's model settings…"),
                Load::Failed(error) => screen::status(error.as_str()),
            },
        }
    }
}

impl AreaMessage for Message {
    type Area = State;
}

impl Area for State {
    const ID: &'static str = "models";
    const COMMANDS: &'static [AreaCommand<Self>] = &[
        AreaCommand::<Self>::new("models.model", "Switch model…", "Models", |state, _, ctx| state.show(Page::Model, ctx)),
        AreaCommand::<Self>::new("models.settings", "Model settings", "Models", |state, _, ctx| state.show(Page::Settings, ctx)),
        AreaCommand::<Self>::new("models.sign-in", "Sign in…", "Models", |state, _, ctx| state.show(Page::SignIn, ctx)),
    ];

    type Message = Message;

    fn new(ctx: &mut Ctx) -> (Self, Task<Message>) {
        let executable = ctx.services.harnesses.get(ctx.services.harnesses.roles_id()).and_then(|harness| harness.executable.clone());
        let launch = OmpLaunch {
            executable: executable.unwrap_or_else(|| OmpLaunch::default().executable),
            cwd: Some(ctx.services.cwd.clone()),
            env: Vec::new(),
        };
        (Self::with_backend(Backend::system(launch)), Task::none())
    }

    fn update(&mut self, message: Message, ctx: &mut Ctx) -> Task<Message> {
        match message {
            Message::Model(model) => self.choose(model.id, model.role, ctx),
            Message::Effort(effort) => self.set_effort(effort, ctx),
            Message::Action(id) => match ACTIONS.iter().find(|(action, _, _)| *action == id) {
                Some((_, _, page)) => self.show(*page, ctx),
                None => Task::none(),
            },
            Message::Show(page) => self.show(page, ctx),
            Message::Close => {
                ctx.close_screen();
                Task::none()
            }
            Message::Listed { harness, result } => {
                let models = result.unwrap_or_default();
                let read_roles = roles(ctx, &harness) && !models.is_empty() && self.roles.is_none();
                // omp's own list (with efforts) replaces the catalog's.
                if !models.is_empty() || !self.listed.contains_key(&harness) {
                    self.listed.insert(harness.clone(), models);
                }
                if self.picker.0 == harness && matches!(self.picker.1, Load::Loading) {
                    if self.listed.get(&harness).is_some_and(|models| !models.is_empty()) {
                        self.open_picker(ctx);
                    } else {
                        let label = ctx.services.harnesses.label(&harness);
                        self.picker.1 = Load::Failed(format!("{label} doesn't list the models it can switch to"));
                    }
                }
                self.sync(ctx);
                if read_roles { self.read_roles() } else { Task::none() }
            }
            Message::Catalog(result) => {
                match result {
                    Ok(catalog) => {
                        self.roles = Some(catalog.roles);
                        let listed = self.listed.entry(ctx.services.harnesses.roles_id().to_owned()).or_default();
                        if listed.is_empty() {
                            *listed = catalog
                                .models
                                .iter()
                                .map(|info| HarnessModel {
                                    id: info.model.selector(),
                                    name: info.display_name().to_owned(),
                                    provider: info.model.provider.clone(),
                                    efforts: Vec::new(),
                                })
                                .collect();
                        }
                        if self.picker.0 == ctx.services.harnesses.roles_id() && matches!(self.picker.1, Load::Loading) {
                            self.open_picker(ctx);
                        }
                    }
                    Err(error) => {
                        if self.picker.0 == ctx.services.harnesses.roles_id() && matches!(self.picker.1, Load::Loading) {
                            self.picker.1 = Load::Failed(format!("Can't read omp's models: {error}"));
                        }
                    }
                }
                self.sync(ctx);
                Task::none()
            }
            Message::ModelSet { session, model, role, result } => {
                match result {
                    Ok(()) => {
                        if let Some(session) = ctx.sessions.get(session.as_str())
                            && let Err(error) = record_role(session, role)
                        {
                            ctx.notify(format!("Switched to {model}, but the role wasn't recorded: {error}"));
                        }
                    }
                    Err(Failure::Unsupported(reason)) => {
                        let harness = ctx.sessions.get(session.as_str()).and_then(Session::harness_id).unwrap_or_else(|| ctx.services.harnesses.roles_id()).to_owned();
                        ctx.notify(format!("{reason}; new sessions start on {model}"));
                        self.pending.locked().insert(None, Pending { harness, model: Some((model, role)), effort: None });
                    }
                    Err(Failure::Error(error)) => ctx.notify(format!("Can't switch to {model}: {error}")),
                }
                self.sync(ctx);
                Task::none()
            }
            Message::EffortSet { session: _, harness, effort, result } => {
                match result {
                    Ok(()) => {}
                    Err(Failure::Unsupported(reason)) => {
                        ctx.notify(format!("{reason}; new sessions start with {effort} effort"));
                        self.pending.locked().entry(None).or_insert_with(|| Pending { harness, ..Pending::default() }).effort = Some(effort);
                    }
                    Err(Failure::Error(error)) => ctx.notify(format!("Can't set {effort} effort: {error}")),
                }
                self.sync(ctx);
                Task::none()
            }
            Message::Providers(providers) => {
                match &mut self.signin {
                    Load::Ready(state) => state.set_providers(providers),
                    load => *load = Load::Ready(signin::State::new(providers)),
                }
                Task::none()
            }
            Message::SignIn(message) => {
                let Load::Ready(state) = &mut self.signin else { return Task::none() };
                state.update(message).map_or_else(Task::none, |event| self.signin_event(event, ctx))
            }
            Message::Login(key, update) => {
                let succeeded = matches!(update, LoginUpdate::Succeeded { .. });
                if let Load::Ready(state) = &mut self.signin {
                    state.login_update(&key, update);
                }
                if !succeeded {
                    return Task::none();
                }
                // The new sign in may bring models and change the rows.
                let providers = self.read_providers();
                Task::batch([providers, self.refresh_models(ctx)])
            }
            Message::Picker(message) => {
                let Load::Ready(picker) = &mut self.picker.1 else { return Task::none() };
                let event = picker.state.update(message);
                let scroll = picker.state.scroll().map(Message::Picker);
                Task::batch([scroll, event.map_or_else(Task::none, |event| self.picker_event(event, ctx))])
            }
            Message::RoleAssigned(result) => match result {
                Ok(()) => self.refresh_models(ctx),
                Err(error) => {
                    ctx.notify(format!("Can't assign the role: {error}"));
                    Task::none()
                }
            },
            Message::SettingsLoaded(result) => {
                self.settings = match result {
                    Ok(load) => Load::Ready(settings::State::new(load.settings, load.known_models)),
                    Err(error) => Load::Failed(format!("Can't read omp's model settings: {error}")),
                };
                Task::none()
            }
            Message::Settings(message) => {
                let Load::Ready(state) = &mut self.settings else { return Task::none() };
                match state.update(message) {
                    Some(settings::Event::Save(settings)) => {
                        let backend = self.backend.clone();
                        Task::perform(async move { backend.save_settings(settings).await }, Message::Saved)
                    }
                    Some(settings::Event::Reverted) | None => Task::none(),
                }
            }
            Message::Saved(result) => {
                let Load::Ready(state) = &mut self.settings else { return Task::none() };
                match result {
                    Ok(settings) => {
                        state.saved(settings);
                        ctx.notify("Model settings saved");
                        self.refresh_models(ctx)
                    }
                    Err(errors) => {
                        state.set_errors(errors);
                        Task::none()
                    }
                }
            }
        }
    }

    fn view_screen<'a>(&'a self, _: &'a Ctx) -> Option<Element<'a, Message>> {
        Some(screen::frame(self.page, self.page_body(self.page), Message::Show, Message::Close))
    }

    fn contribute(&self, config: &mut SessionConfig, target: &Target<'_>, _: &Ctx) {
        let key = target.resume.then(|| config.session_id.clone());
        let mut pending = self.pending.locked();
        let Some(chosen) = pending.get(&key).filter(|pending| pending.harness == target.harness).cloned() else { return };
        pending.remove(&key);
        if let Some((model, role)) = chosen.model {
            config.model = Some(model);
            config.model_role = role;
        }
        if chosen.effort.is_some() {
            config.effort = chosen.effort;
        }
    }

    fn on_session_event(&mut self, event: &SessionEvent, ctx: &mut Ctx) -> Task<Message> {
        let sync = match event {
            SessionEvent::Activated(_) => true,
            SessionEvent::Loaded(id) => ctx.sessions.is_active(id.as_str()),
            // The model, role and effort are attributes of `<session>`.
            SessionEvent::Patch(id, patch) => {
                ctx.sessions.is_active(id.as_str())
                    && !matches!(patch.op, Op::Append { .. })
                    && ctx.sessions.get(id.as_str()).and_then(|session| session.dom().root()).is_some_and(|root| root.id == patch.node)
            }
            _ => false,
        };
        if !sync {
            return Task::none();
        }
        self.sync(ctx);
        self.list_models(ctx)
    }

    fn subscription(&self) -> Subscription<Message> {
        match (&self.page, &self.picker.1) {
            // Arrow keys move the picker's selection; Esc is the app's.
            (Page::Model, Load::Ready(picker)) => picker
                .state
                .subscription()
                .filter_map(|message| (!matches!(message, Msg::Emit(picker::Event::Close))).then_some(message))
                .map(Message::Picker),
            _ => Subscription::none(),
        }
    }
}
