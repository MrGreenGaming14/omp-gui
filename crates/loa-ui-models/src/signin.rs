//! Sign in screen: every provider of every harness with its authentication
//! state, and the sign in flow for subscription (OAuth) logins.
//!
//! The app feeds the provider list (omp `get_login_providers`, plus one row
//! per provider of Claude Code, Codex and OpenCode) and the progress of each
//! running login as [`LoginUpdate`]s. The screen emits [`Event`]s for the app
//! to start, cancel or answer a login. API keys are never entered here: API
//! key rows only say where the harness reads the key from.

use std::collections::HashMap;

use iced::widget::{button, column, container, row, rule, scrollable, space, text, text_input};
use iced::{Element, Fill, Task};

/// Identifies one row: the harness that owns the login and its provider id.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ProviderKey {
    pub harness: String,
    pub provider: String,
}

impl ProviderKey {
    pub fn new(harness: impl Into<String>, provider: impl Into<String>) -> Self {
        Self { harness: harness.into(), provider: provider.into() }
    }
}

/// How a provider authenticates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Auth {
    /// Subscription login (OAuth) run by the harness.
    Login { signed_in: bool, account: Option<String> },
    /// Provider authenticated by an API key the harness reads itself.
    ApiKey {
        /// Where the key was found, e.g. an environment variable name or a
        /// config key. `None` when no key is configured.
        configured_via: Option<String>,
        /// Tells the user where to put the key.
        instructions: String,
    },
}

/// One provider row as the app reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderEntry {
    pub key: ProviderKey,
    /// Display name of the provider, e.g. "Anthropic".
    pub name: String,
    /// Display name of the owning harness, e.g. "omp" or "Claude Code". Rows
    /// are grouped under it.
    pub harness_name: String,
    pub auth: Auth,
}

/// Progress of a running login, fed by the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginUpdate {
    /// The URL the user must open. `launch_url` is a short loopback redirect
    /// and, when present, the recommended copy target.
    Url { url: String, launch_url: Option<String>, instructions: Option<String> },
    Progress(String),
    /// The harness asks for a non-secret value, e.g. a code pasted back from
    /// the browser.
    Prompt { message: String, placeholder: Option<String> },
    Succeeded { account: Option<String> },
    Failed(String),
}

/// What a row shows about its login.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginState {
    Idle,
    /// Sign in requested, waiting for the harness to send the URL. Progress
    /// messages arriving before the URL are kept in `progress` and carried
    /// into [`LoginState::Browser`]; prompts arriving before the URL are
    /// ignored, since the user cannot have a code before visiting the URL.
    Starting { progress: Option<String> },
    /// Browser step: URL shown and copyable, waiting for the user to finish.
    Browser {
        url: String,
        /// The URL the Copy button writes: the launch URL when the harness
        /// sent one, the full URL otherwise.
        copy_url: String,
        instructions: Option<String>,
        /// The latest progress message.
        progress: Option<String>,
        prompt: Option<LoginPrompt>,
    },
    Succeeded { account: Option<String> },
    Failed { message: String },
    Cancelled,
}

impl LoginState {
    /// Whether a login is running for the row.
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Starting { .. } | Self::Browser { .. })
    }
}

/// A pending prompt of a running login and what the user typed so far.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoginPrompt {
    pub message: String,
    pub placeholder: Option<String>,
    pub input: String,
}

#[derive(Clone, Debug)]
pub enum Message {
    SignIn(ProviderKey),
    Cancel(ProviderKey),
    CopyUrl(ProviderKey),
    OpenUrl(ProviderKey),
    PromptInput(ProviderKey, String),
    SubmitPrompt(ProviderKey),
    /// Returns a finished row (succeeded, failed, cancelled) to idle.
    Dismiss(ProviderKey),
}

/// What the app must act on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// Start the login of this provider with its harness.
    SignIn(ProviderKey),
    /// Open this URL in the system browser.
    OpenUrl(String),
    /// Abort the running login of this provider.
    Cancel(ProviderKey),
    /// Answer the running login's prompt.
    SubmitInput { key: ProviderKey, text: String },
}

/// Returned by [`State::login_state`] for rows without a login in progress
/// or finished.
static IDLE: LoginState = LoginState::Idle;

/// The sign in screen.
#[derive(Debug, Default)]
pub struct State {
    providers: Vec<ProviderEntry>,
    /// Login state per row; a missing key means [`LoginState::Idle`].
    logins: HashMap<ProviderKey, LoginState>,
}

impl State {
    pub fn new(providers: Vec<ProviderEntry>) -> Self {
        Self { providers, logins: HashMap::new() }
    }

    /// Replaces the provider list. Login states of rows still present are
    /// kept, so a refreshed list does not interrupt a running login.
    pub fn set_providers(&mut self, providers: Vec<ProviderEntry>) {
        self.logins.retain(|key, _| providers.iter().any(|entry| &entry.key == key));
        self.providers = providers;
    }

    pub fn providers(&self) -> &[ProviderEntry] {
        &self.providers
    }

    pub fn login_state(&self, key: &ProviderKey) -> &LoginState {
        self.logins.get(key).unwrap_or(&IDLE)
    }

    /// Applies a progress update of a running login. Updates for rows with no
    /// running login are ignored, so a late update from a cancelled or
    /// finished flow cannot bring it back.
    pub fn login_update(&mut self, key: &ProviderKey, update: LoginUpdate) {
        let Some(state) = self.logins.get_mut(key) else { return };
        if !state.is_running() {
            return;
        }
        match update {
            LoginUpdate::Url { url, launch_url, instructions } => {
                let progress = match state {
                    LoginState::Starting { progress } | LoginState::Browser { progress, .. } => {
                        progress.take()
                    }
                    _ => None,
                };
                let prompt = match state {
                    LoginState::Browser { prompt, .. } => prompt.take(),
                    _ => None,
                };
                let copy_url = launch_url.unwrap_or_else(|| url.clone());
                *state = LoginState::Browser { url, copy_url, instructions, progress, prompt };
            }
            LoginUpdate::Progress(message) => match state {
                LoginState::Starting { progress } | LoginState::Browser { progress, .. } => {
                    *progress = Some(message);
                }
                _ => {}
            },
            LoginUpdate::Prompt { message, placeholder } => {
                if let LoginState::Browser { prompt, .. } = state {
                    *prompt = Some(LoginPrompt { message, placeholder, input: String::new() });
                }
            }
            LoginUpdate::Succeeded { account } => {
                *state = LoginState::Succeeded { account: account.clone() };
                if let Some(entry) = self.providers.iter_mut().find(|entry| &entry.key == key)
                    && matches!(entry.auth, Auth::Login { .. })
                {
                    entry.auth = Auth::Login { signed_in: true, account };
                }
            }
            LoginUpdate::Failed(message) => *state = LoginState::Failed { message },
        }
    }

    pub fn update(&mut self, message: Message) -> (Task<Message>, Option<Event>) {
        match message {
            Message::SignIn(key) => {
                let is_login = self
                    .entry(&key)
                    .is_some_and(|entry| matches!(entry.auth, Auth::Login { .. }));
                if !is_login || self.login_state(&key).is_running() {
                    return (Task::none(), None);
                }
                self.logins.insert(key.clone(), LoginState::Starting { progress: None });
                (Task::none(), Some(Event::SignIn(key)))
            }
            Message::Cancel(key) => {
                if !self.login_state(&key).is_running() {
                    return (Task::none(), None);
                }
                self.logins.insert(key.clone(), LoginState::Cancelled);
                (Task::none(), Some(Event::Cancel(key)))
            }
            Message::CopyUrl(key) => match self.login_state(&key) {
                LoginState::Browser { copy_url, .. } => {
                    (iced::clipboard::write(copy_url.clone()), None)
                }
                _ => (Task::none(), None),
            },
            Message::OpenUrl(key) => match self.login_state(&key) {
                LoginState::Browser { url, .. } => (Task::none(), Some(Event::OpenUrl(url.clone()))),
                _ => (Task::none(), None),
            },
            Message::PromptInput(key, value) => {
                if let Some(LoginState::Browser { prompt: Some(prompt), .. }) =
                    self.logins.get_mut(&key)
                {
                    prompt.input = value;
                }
                (Task::none(), None)
            }
            Message::SubmitPrompt(key) => {
                let Some(LoginState::Browser { prompt, .. }) = self.logins.get_mut(&key) else {
                    return (Task::none(), None);
                };
                if prompt.as_ref().is_none_or(|prompt| prompt.input.trim().is_empty()) {
                    return (Task::none(), None);
                }
                let text = prompt.take().map(|prompt| prompt.input).unwrap_or_default();
                (Task::none(), Some(Event::SubmitInput { key, text }))
            }
            Message::Dismiss(key) => {
                if !self.login_state(&key).is_running() {
                    self.logins.remove(&key);
                }
                (Task::none(), None)
            }
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let mut groups: Vec<(&str, Vec<&ProviderEntry>)> = Vec::new();
        for entry in &self.providers {
            match groups.iter_mut().find(|(name, _)| *name == entry.harness_name) {
                Some((_, rows)) => rows.push(entry),
                None => groups.push((entry.harness_name.as_str(), vec![entry])),
            }
        }

        let mut list = column![].spacing(16).padding(16);
        if groups.is_empty() {
            list = list.push(text("No providers").style(text::secondary));
        }
        for (harness_name, rows) in groups {
            let mut group = column![text(harness_name).size(20), rule::horizontal(1)].spacing(8);
            for entry in rows {
                group = group.push(self.view_row(entry));
            }
            list = list.push(group);
        }
        scrollable(list).width(Fill).into()
    }

    fn entry(&self, key: &ProviderKey) -> Option<&ProviderEntry> {
        self.providers.iter().find(|entry| &entry.key == key)
    }

    fn view_row<'a>(&'a self, entry: &'a ProviderEntry) -> Element<'a, Message> {
        let key = &entry.key;
        let login = self.login_state(key);
        let mut body = column![].spacing(6);

        match &entry.auth {
            Auth::Login { signed_in, account } => {
                let status = match (signed_in, account) {
                    (true, Some(account)) => text(format!("Signed in as {account}")).style(text::success),
                    (true, None) => text("Signed in").style(text::success),
                    (false, _) => text("Not signed in").style(text::secondary),
                };
                let label = match login {
                    LoginState::Failed { .. } => Some("Retry"),
                    LoginState::Succeeded { .. } => Some("Sign in again"),
                    LoginState::Idle | LoginState::Cancelled if *signed_in => Some("Sign in again"),
                    LoginState::Idle | LoginState::Cancelled => Some("Sign in"),
                    LoginState::Starting { .. } | LoginState::Browser { .. } => None,
                };
                let mut header = row![text(&entry.name), status, space::horizontal()]
                    .spacing(12)
                    .align_y(iced::Center);
                if let Some(label) = label {
                    header = header.push(
                        button(text(label))
                            .style(button::primary)
                            .on_press(Message::SignIn(key.clone())),
                    );
                }
                body = body.push(header);
            }
            Auth::ApiKey { configured_via, instructions } => {
                let status = match configured_via {
                    Some(source) => text(format!("Configured via {source}")).style(text::success),
                    None => text("Not configured").style(text::secondary),
                };
                body = body
                    .push(row![text(&entry.name), status].spacing(12))
                    .push(text(instructions).style(text::secondary));
            }
        }

        if let Some(login) = view_login(key, login) {
            body = body.push(login);
        }
        container(body).style(container::bordered_box).padding(8).width(Fill).into()
    }
}

/// The login flow part of a row; `None` when no login was started.
fn view_login<'a>(key: &ProviderKey, login: &'a LoginState) -> Option<Element<'a, Message>> {
    let cancel = || button(text("Cancel")).style(button::secondary).on_press(Message::Cancel(key.clone()));
    let dismiss = || button(text("Dismiss")).style(button::text).on_press(Message::Dismiss(key.clone()));

    let content: Element<'a, Message> = match login {
        LoginState::Idle => return None,
        LoginState::Starting { progress } => {
            let mut col = column![text("Waiting for the sign in URL…").style(text::secondary)].spacing(6);
            if let Some(progress) = progress {
                col = col.push(text(progress).style(text::secondary));
            }
            col.push(cancel()).into()
        }
        LoginState::Browser { url, instructions, progress, prompt, .. } => {
            let mut col = column![
                text_input("", url),
                row![
                    button(text("Copy")).style(button::secondary).on_press(Message::CopyUrl(key.clone())),
                    button(text("Open browser")).style(button::primary).on_press(Message::OpenUrl(key.clone())),
                ]
                .spacing(8),
            ]
            .spacing(6);
            if let Some(instructions) = instructions {
                col = col.push(text(instructions));
            }
            if let Some(progress) = progress {
                col = col.push(text(progress).style(text::secondary));
            }
            col = col.push(text("Waiting for sign in to finish in the browser…").style(text::secondary));
            if let Some(prompt) = prompt {
                let submit_key = key.clone();
                let input_key = key.clone();
                col = col.push(text(&prompt.message)).push(
                    row![
                        text_input(prompt.placeholder.as_deref().unwrap_or(""), &prompt.input)
                            .on_input(move |value| Message::PromptInput(input_key.clone(), value))
                            .on_submit(Message::SubmitPrompt(submit_key)),
                        button(text("Submit"))
                            .style(button::primary)
                            .on_press_maybe(
                                (!prompt.input.trim().is_empty()).then(|| Message::SubmitPrompt(key.clone())),
                            ),
                    ]
                    .spacing(8),
                );
            }
            col.push(cancel()).into()
        }
        LoginState::Succeeded { account } => {
            let message = match account {
                Some(account) => format!("Signed in as {account}"),
                None => "Signed in".to_owned(),
            };
            row![text(message).style(text::success), dismiss()].spacing(8).align_y(iced::Center).into()
        }
        LoginState::Failed { message } => row![
            text(format!("Sign in failed: {message}")).style(text::danger),
            dismiss()
        ]
        .spacing(8)
        .align_y(iced::Center)
        .into(),
        LoginState::Cancelled => row![text("Sign in cancelled").style(text::secondary), dismiss()]
            .spacing(8)
            .align_y(iced::Center)
            .into(),
    };
    Some(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login_entry(harness: &str, harness_name: &str, provider: &str) -> ProviderEntry {
        ProviderEntry {
            key: ProviderKey::new(harness, provider),
            name: provider.to_owned(),
            harness_name: harness_name.to_owned(),
            auth: Auth::Login { signed_in: false, account: None },
        }
    }

    fn api_key_entry(provider: &str) -> ProviderEntry {
        ProviderEntry {
            key: ProviderKey::new("omp", provider),
            name: provider.to_owned(),
            harness_name: "omp".to_owned(),
            auth: Auth::ApiKey {
                configured_via: None,
                instructions: "Set OPENAI_API_KEY".to_owned(),
            },
        }
    }

    fn omp() -> ProviderKey {
        ProviderKey::new("omp", "anthropic")
    }

    fn url(url: &str, launch_url: Option<&str>) -> LoginUpdate {
        LoginUpdate::Url {
            url: url.to_owned(),
            launch_url: launch_url.map(str::to_owned),
            instructions: None,
        }
    }

    fn started() -> State {
        let mut state = State::new(vec![login_entry("omp", "omp", "anthropic")]);
        let (_task, event) = state.update(Message::SignIn(omp()));
        assert_eq!(event, Some(Event::SignIn(omp())));
        state
    }

    #[test]
    fn happy_path_shows_signed_in_with_account() {
        let mut state = started();
        assert!(matches!(state.login_state(&omp()), LoginState::Starting { .. }));

        state.login_update(&omp(), url("https://auth.example/authorize?x=1", None));
        let (_task, event) = state.update(Message::OpenUrl(omp()));
        assert_eq!(event, Some(Event::OpenUrl("https://auth.example/authorize?x=1".to_owned())));

        state.login_update(&omp(), LoginUpdate::Succeeded { account: Some("me@example.com".to_owned()) });
        assert_eq!(
            state.login_state(&omp()),
            &LoginState::Succeeded { account: Some("me@example.com".to_owned()) }
        );
        assert_eq!(
            state.providers()[0].auth,
            Auth::Login { signed_in: true, account: Some("me@example.com".to_owned()) }
        );
    }

    #[test]
    fn launch_url_is_the_copy_target() {
        let mut state = started();
        state.login_update(&omp(), url("https://auth.example/long", Some("http://localhost:1455/go")));
        match state.login_state(&omp()) {
            LoginState::Browser { url, copy_url, .. } => {
                assert_eq!(url, "https://auth.example/long");
                assert_eq!(copy_url, "http://localhost:1455/go");
            }
            other => panic!("expected browser step, got {other:?}"),
        }
    }

    #[test]
    fn copy_target_falls_back_to_url() {
        let mut state = started();
        state.login_update(&omp(), url("https://auth.example/long", None));
        assert!(matches!(
            state.login_state(&omp()),
            LoginState::Browser { copy_url, .. } if copy_url == "https://auth.example/long"
        ));
    }

    #[test]
    fn progress_before_url_is_carried_into_browser_step() {
        let mut state = started();
        state.login_update(&omp(), LoginUpdate::Progress("Starting local server".to_owned()));
        state.login_update(&omp(), url("https://auth.example", None));
        assert!(matches!(
            state.login_state(&omp()),
            LoginState::Browser { progress: Some(progress), .. } if progress == "Starting local server"
        ));
    }

    #[test]
    fn failure_shows_message_and_retry_signs_in_again() {
        let mut state = started();
        state.login_update(&omp(), url("https://auth.example", None));
        state.login_update(&omp(), LoginUpdate::Failed("token exchange failed".to_owned()));
        assert_eq!(
            state.login_state(&omp()),
            &LoginState::Failed { message: "token exchange failed".to_owned() }
        );
        assert_eq!(state.providers()[0].auth, Auth::Login { signed_in: false, account: None });

        let (_task, event) = state.update(Message::SignIn(omp()));
        assert_eq!(event, Some(Event::SignIn(omp())));
        assert!(matches!(state.login_state(&omp()), LoginState::Starting { .. }));
    }

    #[test]
    fn cancel_emits_cancel_and_late_updates_are_ignored() {
        let mut state = started();
        state.login_update(&omp(), url("https://auth.example", None));
        let (_task, event) = state.update(Message::Cancel(omp()));
        assert_eq!(event, Some(Event::Cancel(omp())));
        assert_eq!(state.login_state(&omp()), &LoginState::Cancelled);

        state.login_update(&omp(), url("https://auth.example/again", None));
        state.login_update(&omp(), LoginUpdate::Succeeded { account: Some("me".to_owned()) });
        assert_eq!(state.login_state(&omp()), &LoginState::Cancelled);
        assert_eq!(state.providers()[0].auth, Auth::Login { signed_in: false, account: None });

        let (_task, event) = state.update(Message::Cancel(omp()));
        assert_eq!(event, None);
    }

    #[test]
    fn updates_for_a_row_never_signed_in_are_ignored() {
        let mut state = State::new(vec![login_entry("omp", "omp", "anthropic")]);
        state.login_update(&omp(), url("https://auth.example", None));
        assert_eq!(state.login_state(&omp()), &LoginState::Idle);
    }

    #[test]
    fn prompt_submit_emits_input_and_clears_prompt() {
        let mut state = started();
        state.login_update(&omp(), url("https://auth.example", None));
        state.login_update(
            &omp(),
            LoginUpdate::Prompt { message: "Paste the code".to_owned(), placeholder: None },
        );

        let (_task, event) = state.update(Message::SubmitPrompt(omp()));
        assert_eq!(event, None, "an empty input submits nothing");

        let (_task, event) = state.update(Message::PromptInput(omp(), "abc123".to_owned()));
        assert_eq!(event, None);
        let (_task, event) = state.update(Message::SubmitPrompt(omp()));
        assert_eq!(event, Some(Event::SubmitInput { key: omp(), text: "abc123".to_owned() }));
        assert!(matches!(state.login_state(&omp()), LoginState::Browser { prompt: None, .. }));

        let (_task, event) = state.update(Message::SubmitPrompt(omp()));
        assert_eq!(event, None, "a cleared prompt cannot be submitted twice");
    }

    #[test]
    fn sign_in_while_running_emits_nothing() {
        let mut state = started();
        let (_task, event) = state.update(Message::SignIn(omp()));
        assert_eq!(event, None);

        state.login_update(&omp(), url("https://auth.example", None));
        let (_task, event) = state.update(Message::SignIn(omp()));
        assert_eq!(event, None);
        assert!(matches!(state.login_state(&omp()), LoginState::Browser { .. }));
    }

    #[test]
    fn api_key_rows_cannot_sign_in() {
        let mut state = State::new(vec![api_key_entry("openai")]);
        let key = ProviderKey::new("omp", "openai");
        let (_task, event) = state.update(Message::SignIn(key.clone()));
        assert_eq!(event, None);
        assert_eq!(state.login_state(&key), &LoginState::Idle);
    }

    #[test]
    fn set_providers_keeps_running_login_and_drops_removed_rows() {
        let mut state = State::new(vec![
            login_entry("omp", "omp", "anthropic"),
            login_entry("omp", "omp", "github-copilot"),
        ]);
        let copilot = ProviderKey::new("omp", "github-copilot");
        let _ = state.update(Message::SignIn(omp()));
        let _ = state.update(Message::SignIn(copilot.clone()));
        state.login_update(&omp(), url("https://auth.example", None));

        state.set_providers(vec![login_entry("omp", "omp", "anthropic")]);
        assert!(matches!(state.login_state(&omp()), LoginState::Browser { .. }));
        assert_eq!(state.login_state(&copilot), &LoginState::Idle);

        state.login_update(&omp(), LoginUpdate::Succeeded { account: None });
        assert_eq!(state.providers()[0].auth, Auth::Login { signed_in: true, account: None });
    }

    #[test]
    fn same_provider_in_two_harnesses_keeps_separate_states() {
        let claude = ProviderKey::new("claude-code", "anthropic");
        let mut state = State::new(vec![
            login_entry("omp", "omp", "anthropic"),
            login_entry("claude-code", "Claude Code", "anthropic"),
        ]);
        let (_task, event) = state.update(Message::SignIn(claude.clone()));
        assert_eq!(event, Some(Event::SignIn(claude.clone())));
        assert_eq!(state.login_state(&omp()), &LoginState::Idle);

        state.login_update(&claude, LoginUpdate::Succeeded { account: Some("me".to_owned()) });
        assert_eq!(state.providers()[0].auth, Auth::Login { signed_in: false, account: None });
        assert_eq!(
            state.providers()[1].auth,
            Auth::Login { signed_in: true, account: Some("me".to_owned()) }
        );
    }

    #[test]
    fn dismiss_returns_finished_row_to_idle_but_not_a_running_one() {
        let mut state = started();
        let _ = state.update(Message::Dismiss(omp()));
        assert!(matches!(state.login_state(&omp()), LoginState::Starting { .. }));

        state.login_update(&omp(), LoginUpdate::Failed("denied".to_owned()));
        let _ = state.update(Message::Dismiss(omp()));
        assert_eq!(state.login_state(&omp()), &LoginState::Idle);
    }
}
