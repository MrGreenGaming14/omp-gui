//! Sign in: every provider of every harness with its authentication state, and the sign in flow
//! of subscription logins as a pure reducer over [`LoginUpdate`]s. API keys are never entered
//! here; API key rows only say where the harness reads the key from.

use crate::{action, dot, field, section};
use iced::widget::{column, row, text_input};
use iced::{Alignment, Element, Fill};
use loa_ui_kit::style::{ButtonKind, Tone};
use loa_ui_kit::theme::{SpaceRole, TextRole, sp};
use loa_ui_kit::widgets::{Msg, Screen, label, mono_block};
use std::collections::HashMap;

use loa_view::settings::models::signin::Intent;
use loa_view::settings::models::{Auth, LoginPrompt, LoginState, LoginUpdate, ProviderEntry, ProviderKey};

#[derive(Clone, Debug)]
pub enum Local {
    SignIn(ProviderKey),
    Cancel(ProviderKey),
    /// Text typed into a running login's prompt.
    Input(ProviderKey, String),
    Submit(ProviderKey),
    /// Returns a finished row (succeeded, failed, cancelled) to idle.
    Dismiss(ProviderKey),
}

pub type Message = Msg<Local, Intent>;

static IDLE: LoginState = LoginState::Idle;

/// The provider rows and the login state of each; a missing key is idle.
#[derive(Debug, Default)]
pub struct State {
    providers: Vec<ProviderEntry>,
    logins: HashMap<ProviderKey, LoginState>,
}

impl State {
    pub fn new(providers: Vec<ProviderEntry>) -> Self { Self { providers, logins: HashMap::new() } }

    /// Replaces the rows; logins of rows still present go on.
    pub fn set_providers(&mut self, providers: Vec<ProviderEntry>) {
        self.logins.retain(|key, _| providers.iter().any(|e| &e.key == key));
        self.providers = providers;
    }

    pub fn providers(&self) -> &[ProviderEntry] { &self.providers }

    pub fn login_state(&self, key: &ProviderKey) -> &LoginState { self.logins.get(key).unwrap_or(&IDLE) }

    /// Applies a running login's progress. Rows without a running login ignore it, so a late
    /// update of a cancelled or finished login cannot bring it back.
    pub fn login_update(&mut self, key: &ProviderKey, update: LoginUpdate) {
        let Some(state) = self.logins.get_mut(key).filter(|s| s.is_running()) else { return };
        let (progress, prompt) = match state {
            LoginState::Starting { progress } => (progress, None),
            LoginState::Browser { progress, prompt, .. } => (progress, Some(prompt)),
            _ => unreachable!("a running login starts or waits in the browser"),
        };
        match update {
            LoginUpdate::Url { url, launch_url, instructions } => {
                let (progress, prompt) = (progress.take(), prompt.and_then(Option::take));
                *state = LoginState::Browser { copy_url: launch_url.unwrap_or_else(|| url.clone()), url, instructions, progress, prompt };
            }
            LoginUpdate::Progress(message) => *progress = Some(message),
            // The user cannot have a code before visiting the URL.
            LoginUpdate::Prompt { message, placeholder } => {
                if let Some(prompt) = prompt {
                    *prompt = Some(LoginPrompt { message, placeholder, input: String::new() });
                }
            }
            LoginUpdate::Succeeded { account } => {
                *state = LoginState::Succeeded { account: account.clone() };
                if let Some(entry) = self.providers.iter_mut().find(|e| &e.key == key && matches!(e.auth, Auth::Login { .. })) {
                    entry.auth = Auth::Login { signed_in: true, account };
                }
            }
            LoginUpdate::Failed(message) => *state = LoginState::Failed { message },
            LoginUpdate::Cancelled => *state = LoginState::Cancelled,
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        let mut harnesses: Vec<&str> = self.providers.iter().map(|e| e.harness_name.as_str()).collect();
        harnesses.dedup();
        let groups = harnesses.into_iter().map(|h| section(h, column(self.providers.iter().filter(|e| e.harness_name == h).map(|e| self.row(e))).spacing(sp(SpaceRole::M))));
        let list = column(groups).spacing(sp(SpaceRole::L));
        crate::scroll(if self.providers.is_empty() { column![label("No providers", TextRole::Label, Tone::Muted)] } else { list })
    }

    fn row<'a>(&'a self, entry: &'a ProviderEntry) -> Element<'a, Message> {
        let key = &entry.key;
        let login = self.login_state(key);
        let (on, status, detail) = match &entry.auth {
            Auth::Login { signed_in, account } => (*signed_in, account.as_ref().map_or_else(|| if *signed_in { "Signed in".to_owned() } else { "Not signed in".to_owned() }, |a| format!("Signed in as {a}")), None),
            Auth::ApiKey { configured_via, instructions } => (configured_via.is_some(), configured_via.as_ref().map_or_else(|| "Not configured".to_owned(), |s| format!("Configured via {s}")), Some(instructions)),
            Auth::Unavailable { reason } => (false, "Unavailable".to_owned(), Some(reason)),
        };
        let sign_in = match (&entry.auth, login) {
            (Auth::Login { .. }, LoginState::Failed { .. }) => Some("Retry"),
            (Auth::Login { .. }, LoginState::Succeeded { .. }) | (Auth::Login { signed_in: true, .. }, LoginState::Idle | LoginState::Cancelled) => Some("Sign in again"),
            (Auth::Login { .. }, LoginState::Idle | LoginState::Cancelled) => Some("Sign in"),
            _ => None,
        };
        let head = row![dot(if on { Tone::Ok } else { Tone::Faint }), label(&entry.name, TextRole::Body, Tone::Normal), label(status, TextRole::Caption, if on { Tone::Ok } else { Tone::Muted })];
        let head = head.push(sign_in.map(|s| action(s, ButtonKind::Ghost, Some(Msg::Local(Local::SignIn(key.clone()))))));
        let detail = detail.map(|d| label(d, TextRole::Caption, Tone::Muted));
        column![head.spacing(sp(SpaceRole::S)).align_y(Alignment::Center), detail, login_view(key, login)].spacing(sp(SpaceRole::Xs)).width(Fill).into()
    }
}

/// The login flow part of a row; nothing while idle.
fn login_view<'a>(key: &ProviderKey, login: &'a LoginState) -> Option<Element<'a, Message>> {
    let local = |l: fn(ProviderKey) -> Local| Some(Msg::Local(l(key.clone())));
    let note = |s: String, tone| label(s, TextRole::Label, tone);
    let (lines, button) = match login {
        LoginState::Idle => return None,
        LoginState::Starting { progress } => (column![note("Waiting for the sign in URL…".into(), Tone::Muted)].push(progress.as_ref().map(|p| note(p.clone(), Tone::Muted))), action("Cancel", ButtonKind::Ghost, local(Local::Cancel))),
        LoginState::Browser { url, copy_url, instructions, progress, prompt } => {
            let open = row![action("Copy", ButtonKind::Ghost, Some(Msg::Emit(Intent::CopyUrl(copy_url.clone())))), action("Open browser", ButtonKind::Primary, Some(Msg::Emit(Intent::OpenUrl(url.clone()))))];
            let prompt = prompt.as_ref().map(|p| {
                let submit = (!p.input.trim().is_empty()).then(|| Msg::Local(Local::Submit(key.clone())));
                let input_key = key.clone();
                let input: text_input::TextInput<'a, Message> = field(p.placeholder.as_deref().unwrap_or(""), &p.input, move |v| Msg::Local(Local::Input(input_key.clone(), v)));
                column![note(p.message.clone(), Tone::Normal), row![input.on_submit_maybe(submit.clone()), action("Submit", ButtonKind::Primary, submit)].spacing(sp(SpaceRole::S))].spacing(sp(SpaceRole::Xs))
            });
            let lines = column![mono_block(url.as_str()), open.spacing(sp(SpaceRole::S))].push(instructions.as_ref().map(|i| note(i.clone(), Tone::Normal))).push(progress.as_ref().map(|p| note(p.clone(), Tone::Muted)));
            (lines.push(note("Waiting for sign in to finish in the browser…".into(), Tone::Muted)).push(prompt), action("Cancel", ButtonKind::Ghost, local(Local::Cancel)))
        }
        LoginState::Succeeded { account } => (column![note(account.as_ref().map_or_else(|| "Signed in".to_owned(), |a| format!("Signed in as {a}")), Tone::Ok)], action("Dismiss", ButtonKind::Ghost, local(Local::Dismiss))),
        LoginState::Failed { message } => (column![note(format!("Sign in failed: {message}"), Tone::Danger)], action("Dismiss", ButtonKind::Ghost, local(Local::Dismiss))),
        LoginState::Cancelled => (column![note("Sign in cancelled".into(), Tone::Muted)], action("Dismiss", ButtonKind::Ghost, local(Local::Dismiss))),
    };
    Some(lines.push(button).spacing(sp(SpaceRole::Xs)).into())
}

impl Screen for State {
    type Local = Local;
    type Event = Intent;

    fn reduce(&mut self, m: Local) -> Option<Intent> {
        match m {
            Local::SignIn(key) => {
                let login = self.providers.iter().any(|e| e.key == key && matches!(e.auth, Auth::Login { .. }));
                (login && !self.login_state(&key).is_running()).then(|| {
                    self.logins.insert(key.clone(), LoginState::Starting { progress: None });
                    Intent::SignIn(key)
                })
            }
            Local::Cancel(key) => self.login_state(&key).is_running().then(|| {
                self.logins.insert(key.clone(), LoginState::Cancelled);
                Intent::Cancel(key)
            }),
            Local::Input(key, value) => {
                if let Some(LoginState::Browser { prompt: Some(p), .. }) = self.logins.get_mut(&key) {
                    p.input = value;
                }
                None
            }
            Local::Submit(key) => {
                let Some(LoginState::Browser { prompt, .. }) = self.logins.get_mut(&key) else { return None };
                let text = prompt.take_if(|p| !p.input.trim().is_empty())?.input;
                Some(Intent::SubmitInput { key, text })
            }
            Local::Dismiss(key) => {
                if !self.login_state(&key).is_running() {
                    self.logins.remove(&key);
                }
                None
            }
        }
    }

    /// Only the URLs of a login at its browser step leave the screen.
    fn guard(&self, e: &Intent) -> bool {
        let shown = |pick: fn(&LoginState) -> Option<&String>, u: &String| self.logins.values().any(|l| pick(l) == Some(u));
        match e {
            Intent::OpenUrl(u) => shown(|l| if let LoginState::Browser { url, .. } = l { Some(url) } else { None }, u),
            Intent::CopyUrl(u) => shown(|l| if let LoginState::Browser { copy_url, .. } = l { Some(copy_url) } else { None }, u),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_ui_kit::testkit::click_label;

    fn key(harness: &str) -> ProviderKey { ProviderKey::new(harness, "anthropic") }
    fn row(harness: &str, auth: Auth) -> ProviderEntry { ProviderEntry { key: key(harness), name: "anthropic".into(), harness_name: harness.into(), auth } }
    fn signed_out() -> Auth { Auth::Login { signed_in: false, account: None } }
    fn url(launch: Option<&str>) -> LoginUpdate { LoginUpdate::Url { url: "https://auth.example".into(), launch_url: launch.map(Into::into), instructions: None } }
    fn local(l: Local) -> Message { Msg::Local(l) }
    fn omp() -> ProviderKey { key("omp") }
    type Check = fn(&State) -> bool;

    #[test]
    fn logins_reduce_messages_and_updates() {
        let browser = |s: &State| matches!(s.login_state(&omp()), LoginState::Browser { .. });
        let start = || local(Local::SignIn(omp()));
        // (messages and updates, emitted events, state afterwards)
        type Case = (Vec<Result<Message, LoginUpdate>>, Vec<Intent>, Check);
        let cases: Vec<Case> = vec![
            (vec![Ok(start()), Ok(start())], vec![Intent::SignIn(omp())], |s| matches!(s.login_state(&omp()), LoginState::Starting { progress: None })),
            (vec![Ok(start()), Err(LoginUpdate::Progress("local server".into())), Err(url(None))], vec![Intent::SignIn(omp())], |s| matches!(s.login_state(&omp()), LoginState::Browser { progress: Some(p), copy_url, .. } if p == "local server" && copy_url == "https://auth.example")),
            (vec![Ok(start()), Err(url(Some("http://localhost/go"))), Ok(Msg::Emit(Intent::CopyUrl("http://localhost/go".into()))), Ok(Msg::Emit(Intent::OpenUrl("https://evil.example".into())))], vec![Intent::SignIn(omp()), Intent::CopyUrl("http://localhost/go".into())], browser),
            (vec![Ok(start()), Err(url(None)), Err(LoginUpdate::Succeeded { account: Some("me".into()) })], vec![Intent::SignIn(omp())], |s| s.providers[0].auth == Auth::Login { signed_in: true, account: Some("me".into()) }),
            (vec![Ok(start()), Err(url(None)), Ok(local(Local::Cancel(omp()))), Err(LoginUpdate::Succeeded { account: None }), Ok(local(Local::Cancel(omp())))], vec![Intent::SignIn(omp()), Intent::Cancel(omp())], |s| *s.login_state(&omp()) == LoginState::Cancelled && s.providers[0].auth == signed_out()),
            (vec![Err(url(None)), Ok(local(Local::SignIn(key("codex"))))], vec![], |s| *s.login_state(&omp()) == LoginState::Idle),
            (vec![Ok(start()), Err(LoginUpdate::Failed("denied".into())), Ok(start())], vec![Intent::SignIn(omp()), Intent::SignIn(omp())], |s| matches!(s.login_state(&omp()), LoginState::Starting { .. })),
            (vec![Ok(start()), Ok(local(Local::Dismiss(omp()))), Err(LoginUpdate::Failed("denied".into())), Ok(local(Local::Dismiss(omp())))], vec![Intent::SignIn(omp())], |s| *s.login_state(&omp()) == LoginState::Idle),
            (vec![Ok(local(Local::SignIn(key("claude-code"))))], vec![Intent::SignIn(key("claude-code"))], |s| *s.login_state(&omp()) == LoginState::Idle),
            (vec![Ok(start()), Err(url(None)), Err(LoginUpdate::Prompt { message: "Paste the code".into(), placeholder: None }), Ok(local(Local::Submit(omp()))), Ok(local(Local::Input(omp(), "abc".into()))), Ok(local(Local::Submit(omp()))), Ok(local(Local::Submit(omp())))],
                vec![Intent::SignIn(omp()), Intent::SubmitInput { key: omp(), text: "abc".into() }], |s| matches!(s.login_state(&omp()), LoginState::Browser { prompt: None, .. })),
        ];
        for (i, (steps, want, check)) in cases.into_iter().enumerate() {
            let mut s = State::new(vec![row("omp", signed_out()), row("claude-code", signed_out()), row("codex", Auth::ApiKey { configured_via: None, instructions: "Set OPENAI_API_KEY".into() })]);
            let mut events = Vec::new();
            for step in steps {
                match step {
                    Ok(m) => events.extend(s.update(m)),
                    Err(u) => s.login_update(&omp(), u),
                }
            }
            assert_eq!(events, want, "case {i}");
            assert!(check(&s), "case {i}: {:?}", s.logins);
        }
    }

    #[test]
    fn refreshed_rows_keep_running_logins() {
        let mut s = State::new(vec![row("omp", signed_out()), row("claude-code", signed_out())]);
        for k in [omp(), key("claude-code")] {
            s.update(local(Local::SignIn(k)));
        }
        s.set_providers(vec![row("omp", signed_out())]);
        assert!(s.login_state(&omp()).is_running());
        assert_eq!(*s.login_state(&key("claude-code")), LoginState::Idle);
    }

    #[test]
    fn the_view_signs_in_and_opens_the_login_url() {
        let mut s = State::new(vec![row("omp", signed_out())]);
        let events: Vec<Intent> = click_label(s.view(), "Sign in").into_iter().filter_map(|m| s.update(m)).collect();
        assert_eq!(events, [Intent::SignIn(omp())]);
        s.login_update(&omp(), url(None));
        let events: Vec<Intent> = click_label(s.view(), "Open browser").into_iter().filter_map(|m| s.update(m)).collect();
        assert_eq!(events, [Intent::OpenUrl("https://auth.example".into())]);
    }
}
