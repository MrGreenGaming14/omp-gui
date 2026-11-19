//! The OAuth login flow behind the sign in button.
//!
//! omp runs the flow itself. Over RPC the `login` command sends `open_url` (the
//! authorization URL), `notify` (progress) and, after the URL, non-secret
//! `input` requests, then answers with success or failure. [`LoginFlow`] is the
//! state machine the sign in screen renders; [`run_login`] drives it against an
//! [`OmpRpc`] and opens the browser through a [`UrlOpener`].
//!
//! omp's RPC has no command to abort a running login. Cancelling stops waiting
//! for it, answers a pending input request with `cancelled`, and leaves omp's
//! own flow to time out.

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::rpc::{LoginProvider, LoginUiRequest, OmpRpc, RpcError, UiResponse};
use crate::{Error, Result};

/// Opens a URL in the user's browser. The app implements it with its platform
/// opener; it must not block on the browser.
pub trait UrlOpener: Send + Sync {
    /// Opens `url`, returning an error message when no browser could be launched.
    fn open_url(&self, url: &str) -> std::result::Result<(), String>;
}

/// What the sign in screen shows for a login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginState {
    /// No login has started.
    Idle,
    /// The `login` command was sent; omp has not produced a URL yet.
    Starting {
        provider: String,
        progress: Option<String>,
    },
    /// The browser was sent to the authorization URL; omp waits for the
    /// provider's callback.
    WaitingForBrowser {
        provider: String,
        /// The URL that was opened.
        url: String,
        /// The URL to offer for copying: omp's short loopback `launchUrl` when it
        /// sent one, else `url`.
        copy_url: String,
        instructions: Option<String>,
        progress: Option<String>,
        /// Set when the browser could not be opened, so the user copies the URL.
        open_error: Option<String>,
    },
    /// omp asks the user to type something (for example the callback URL).
    WaitingForInput {
        provider: String,
        request_id: String,
        title: String,
        placeholder: Option<String>,
        /// The authorization URL, still shown while the user types.
        copy_url: Option<String>,
    },
    /// omp stored the credential.
    Succeeded { provider: String },
    /// The login failed; `message` is omp's error.
    Failed { provider: String, message: String },
    /// The user cancelled.
    Cancelled { provider: String },
}

impl LoginState {
    /// Whether a login is in progress.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            Self::Starting { .. } | Self::WaitingForBrowser { .. } | Self::WaitingForInput { .. }
        )
    }

    fn provider(&self) -> Option<&str> {
        match self {
            Self::Idle => None,
            Self::Starting { provider, .. }
            | Self::WaitingForBrowser { provider, .. }
            | Self::WaitingForInput { provider, .. }
            | Self::Succeeded { provider }
            | Self::Failed { provider, .. }
            | Self::Cancelled { provider } => Some(provider),
        }
    }
}

/// A side effect the caller must perform after a transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginEffect {
    /// Open this URL in the browser, then report failure with
    /// [`LoginFlow::on_open_failed`].
    OpenUrl(String),
    /// Send this `extension_ui_response` to omp.
    Respond(UiResponse),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BrowserStep {
    url: String,
    copy_url: String,
    instructions: Option<String>,
    open_error: Option<String>,
}

/// The login state machine.
#[derive(Debug, Clone)]
pub struct LoginFlow {
    state: LoginState,
    browser: Option<BrowserStep>,
    progress: Option<String>,
}

impl Default for LoginFlow {
    fn default() -> Self {
        Self::new()
    }
}

impl LoginFlow {
    /// A flow with no login started.
    pub fn new() -> Self {
        Self {
            state: LoginState::Idle,
            browser: None,
            progress: None,
        }
    }

    /// The current state.
    pub fn state(&self) -> &LoginState {
        &self.state
    }

    /// Starts a login for `provider`. Refused while another login runs or when
    /// omp offers no login for the provider.
    pub fn start(&mut self, provider: &LoginProvider) -> Result<()> {
        if self.state.is_active() {
            return Err(Error::invalid(
                "login",
                format!(
                    "a login for {} is already running",
                    self.state.provider().unwrap_or_default()
                ),
            ));
        }
        if !provider.available {
            return Err(Error::invalid(
                "login",
                format!("omp offers no login for {} now", provider.name),
            ));
        }
        self.browser = None;
        self.progress = None;
        self.state = LoginState::Starting {
            provider: provider.id.clone(),
            progress: None,
        };
        Ok(())
    }

    /// Applies a UI frame omp sent during the login.
    pub fn on_ui(&mut self, request: LoginUiRequest) -> Option<LoginEffect> {
        if !self.state.is_active() {
            return None;
        }
        match request {
            LoginUiRequest::OpenUrl {
                url,
                launch_url,
                instructions,
                ..
            } => {
                self.browser = Some(BrowserStep {
                    copy_url: launch_url.unwrap_or_else(|| url.clone()),
                    url: url.clone(),
                    instructions,
                    open_error: None,
                });
                self.state = self.waiting_state();
                Some(LoginEffect::OpenUrl(url))
            }
            LoginUiRequest::Notify { message, .. } => {
                self.progress = Some(message);
                match &mut self.state {
                    LoginState::Starting { progress, .. } => *progress = self.progress.clone(),
                    LoginState::WaitingForBrowser { progress, .. } => {
                        *progress = self.progress.clone();
                    }
                    _ => {}
                }
                None
            }
            LoginUiRequest::Input {
                id,
                title,
                placeholder,
                ..
            } => {
                self.state = LoginState::WaitingForInput {
                    provider: self.provider_id(),
                    request_id: id,
                    title,
                    placeholder,
                    copy_url: self.browser.as_ref().map(|b| b.copy_url.clone()),
                };
                None
            }
            LoginUiRequest::Cancel { target_id, .. } => {
                if matches!(&self.state, LoginState::WaitingForInput { request_id, .. } if *request_id == target_id)
                {
                    self.state = self.waiting_state();
                }
                None
            }
        }
    }

    /// Records that the browser could not be opened, so the screen shows the URL
    /// to copy.
    pub fn on_open_failed(&mut self, message: String) {
        if let Some(step) = &mut self.browser {
            step.open_error = Some(message);
        }
        if matches!(self.state, LoginState::WaitingForBrowser { .. }) {
            self.state = self.waiting_state();
        }
    }

    /// Answers the pending input request with `value`.
    pub fn submit_input(&mut self, value: String) -> Option<LoginEffect> {
        let LoginState::WaitingForInput { request_id, .. } = &self.state else {
            return None;
        };
        let response = UiResponse::Value {
            id: request_id.clone(),
            value,
        };
        self.state = self.waiting_state();
        Some(LoginEffect::Respond(response))
    }

    /// Cancels the login. Returns the response that withdraws a pending input.
    pub fn cancel(&mut self) -> Option<LoginEffect> {
        if !self.state.is_active() {
            return None;
        }
        let effect = match &self.state {
            LoginState::WaitingForInput { request_id, .. } => {
                Some(LoginEffect::Respond(UiResponse::Cancelled {
                    id: request_id.clone(),
                    cancelled: true,
                }))
            }
            _ => None,
        };
        self.state = LoginState::Cancelled {
            provider: self.provider_id(),
        };
        effect
    }

    /// Applies the `login` command's response.
    pub fn on_finished(&mut self, result: std::result::Result<String, RpcError>) {
        if !self.state.is_active() {
            return;
        }
        let provider = self.provider_id();
        self.state = match result {
            Ok(_) => LoginState::Succeeded { provider },
            Err(RpcError::Command { message, .. }) => LoginState::Failed { provider, message },
            Err(err) => LoginState::Failed {
                provider,
                message: err.to_string(),
            },
        };
    }

    fn provider_id(&self) -> String {
        self.state.provider().unwrap_or_default().to_owned()
    }

    fn waiting_state(&self) -> LoginState {
        let provider = self.provider_id();
        match &self.browser {
            Some(step) => LoginState::WaitingForBrowser {
                provider,
                url: step.url.clone(),
                copy_url: step.copy_url.clone(),
                instructions: step.instructions.clone(),
                progress: self.progress.clone(),
                open_error: step.open_error.clone(),
            },
            None => LoginState::Starting {
                provider,
                progress: self.progress.clone(),
            },
        }
    }
}

/// What the sign in screen sends to a running [`run_login`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginControl {
    /// The text the user typed for the pending input request.
    SubmitInput(String),
    /// Stop the login.
    Cancel,
}

/// Runs one login to its end: sends `login`, opens the browser when omp sends
/// the URL, relays input and cancellation from `controls`, and publishes every
/// state on `states`. Returns the final state.
pub async fn run_login(
    rpc: &dyn OmpRpc,
    opener: &dyn UrlOpener,
    provider: &LoginProvider,
    mut controls: UnboundedReceiver<LoginControl>,
    states: UnboundedSender<LoginState>,
) -> LoginState {
    let mut flow = LoginFlow::new();
    if let Err(err) = flow.start(provider) {
        let failed = LoginState::Failed {
            provider: provider.id.clone(),
            message: err.to_string(),
        };
        let _ = states.send(failed.clone());
        return failed;
    }
    let _ = states.send(flow.state().clone());

    let (ui_tx, mut ui_rx) = unbounded_channel();
    let login = rpc.login(&provider.id, ui_tx);
    let mut login = std::pin::pin!(login);
    let mut controls_open = true;
    loop {
        tokio::select! {
            biased;
            Some(request) = ui_rx.recv() => {
                if let Some(effect) = flow.on_ui(request) {
                    apply(rpc, opener, &mut flow, effect).await;
                }
            }
            control = controls.recv(), if controls_open => match control {
                Some(LoginControl::SubmitInput(value)) => {
                    if let Some(effect) = flow.submit_input(value) {
                        apply(rpc, opener, &mut flow, effect).await;
                    }
                }
                Some(LoginControl::Cancel) => {
                    let effect = flow.cancel();
                    let _ = states.send(flow.state().clone());
                    if let Some(effect) = effect {
                        apply(rpc, opener, &mut flow, effect).await;
                    }
                    return flow.state().clone();
                }
                None => controls_open = false,
            },
            result = &mut login => {
                while let Ok(request) = ui_rx.try_recv() {
                    if let Some(effect) = flow.on_ui(request) {
                        apply(rpc, opener, &mut flow, effect).await;
                    }
                    let _ = states.send(flow.state().clone());
                }
                flow.on_finished(result);
                let _ = states.send(flow.state().clone());
                return flow.state().clone();
            }
        }
        let _ = states.send(flow.state().clone());
    }
}

async fn apply(rpc: &dyn OmpRpc, opener: &dyn UrlOpener, flow: &mut LoginFlow, effect: LoginEffect) {
    match effect {
        LoginEffect::OpenUrl(url) => {
            if let Err(message) = opener.open_url(&url) {
                flow.on_open_failed(message);
            }
        }
        LoginEffect::Respond(response) => {
            if let Err(err) = rpc.respond_ui(response).await {
                flow.on_finished(Err(err));
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::Mutex;

    use tokio::sync::mpsc;

    use super::*;
    use crate::rpc::ModelInfo;

    /// What the fake omp does when `login` is called.
    #[derive(Clone)]
    pub(crate) enum Script {
        /// Send these frames, then answer with this result.
        Frames(Vec<LoginUiRequest>, std::result::Result<String, RpcError>),
        /// Send these frames, wait for an input answer, succeed when it equals
        /// the expected value.
        AwaitInput(Vec<LoginUiRequest>, String),
        /// Send these frames and never answer.
        Hang(Vec<LoginUiRequest>),
    }

    pub(crate) struct FakeOmp {
        pub script: Script,
        pub responses: Mutex<Vec<UiResponse>>,
        response_tx: mpsc::UnboundedSender<UiResponse>,
        response_rx: tokio::sync::Mutex<mpsc::UnboundedReceiver<UiResponse>>,
        pub providers: Vec<LoginProvider>,
        pub models: Vec<ModelInfo>,
        pub selected: Mutex<Vec<(String, String)>>,
    }

    impl FakeOmp {
        pub(crate) fn new(script: Script) -> Self {
            let (response_tx, response_rx) = mpsc::unbounded_channel();
            Self {
                script,
                responses: Mutex::new(Vec::new()),
                response_tx,
                response_rx: tokio::sync::Mutex::new(response_rx),
                providers: Vec::new(),
                models: Vec::new(),
                selected: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait::async_trait]
    impl OmpRpc for FakeOmp {
        async fn get_login_providers(&self) -> std::result::Result<Vec<LoginProvider>, RpcError> {
            Ok(self.providers.clone())
        }

        async fn login(
            &self,
            _provider_id: &str,
            ui: UnboundedSender<LoginUiRequest>,
        ) -> std::result::Result<String, RpcError> {
            match self.script.clone() {
                Script::Frames(frames, result) => {
                    for f in frames {
                        let _ = ui.send(f);
                    }
                    result
                }
                Script::AwaitInput(frames, expected) => {
                    for f in frames {
                        let _ = ui.send(f);
                    }
                    let got = self.response_rx.lock().await.recv().await;
                    match got {
                        Some(UiResponse::Value { value, .. }) if value == expected => {
                            Ok("anthropic".into())
                        }
                        _ => Err(RpcError::Command {
                            command: "login".into(),
                            message: "bad callback".into(),
                            code: None,
                        }),
                    }
                }
                Script::Hang(frames) => {
                    for f in frames {
                        let _ = ui.send(f);
                    }
                    std::future::pending().await
                }
            }
        }

        async fn respond_ui(&self, response: UiResponse) -> std::result::Result<(), RpcError> {
            self.responses.lock().unwrap().push(response.clone());
            let _ = self.response_tx.send(response);
            Ok(())
        }

        async fn get_available_models(&self) -> std::result::Result<Vec<ModelInfo>, RpcError> {
            Ok(self.models.clone())
        }

        async fn set_model(
            &self,
            provider: &str,
            model_id: &str,
        ) -> std::result::Result<ModelInfo, RpcError> {
            self.selected
                .lock()
                .unwrap()
                .push((provider.to_owned(), model_id.to_owned()));
            self.models
                .iter()
                .find(|m| m.provider == provider && m.id == model_id)
                .cloned()
                .ok_or_else(|| RpcError::Command {
                    command: "set_model".into(),
                    message: format!("Model not found: {provider}/{model_id}"),
                    code: None,
                })
        }
    }

    #[derive(Default)]
    struct RecordingOpener {
        opened: Mutex<Vec<String>>,
        fail: bool,
    }

    impl UrlOpener for RecordingOpener {
        fn open_url(&self, url: &str) -> std::result::Result<(), String> {
            self.opened.lock().unwrap().push(url.to_owned());
            if self.fail {
                Err("no browser".into())
            } else {
                Ok(())
            }
        }
    }

    fn anthropic() -> LoginProvider {
        LoginProvider {
            id: "anthropic".into(),
            name: "Anthropic".into(),
            available: true,
            authenticated: false,
        }
    }

    fn open_url(launch: Option<&str>) -> LoginUiRequest {
        LoginUiRequest::OpenUrl {
            id: "1".into(),
            url: "https://claude.example/oauth/authorize?code=true&state=abc".into(),
            launch_url: launch.map(str::to_owned),
            instructions: None,
        }
    }

    fn input() -> LoginUiRequest {
        LoginUiRequest::Input {
            id: "2".into(),
            title: "Paste the authorization code".into(),
            placeholder: None,
            timeout: Some(600_000),
        }
    }

    #[tokio::test]
    async fn successful_login_opens_the_url_and_ends_signed_in() {
        let omp = FakeOmp::new(Script::Frames(
            vec![
                LoginUiRequest::Notify {
                    id: "0".into(),
                    message: "Starting".into(),
                    notify_type: None,
                },
                open_url(None),
            ],
            Ok("anthropic".into()),
        ));
        let opener = RecordingOpener::default();
        let (_ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let (st_tx, mut st_rx) = mpsc::unbounded_channel();
        let end = run_login(&omp, &opener, &anthropic(), ctl_rx, st_tx).await;
        assert_eq!(
            end,
            LoginState::Succeeded {
                provider: "anthropic".into()
            }
        );
        assert_eq!(
            *opener.opened.lock().unwrap(),
            ["https://claude.example/oauth/authorize?code=true&state=abc"]
        );
        let mut seen_browser = false;
        while let Ok(s) = st_rx.try_recv() {
            seen_browser |= matches!(s, LoginState::WaitingForBrowser { .. });
        }
        assert!(seen_browser);
    }

    #[tokio::test]
    async fn pasted_code_is_sent_back_to_omp() {
        let omp = FakeOmp::new(Script::AwaitInput(
            vec![open_url(None), input()],
            "code#state".into(),
        ));
        let opener = RecordingOpener::default();
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let (st_tx, mut st_rx) = mpsc::unbounded_channel();
        let provider = anthropic();
        let run = run_login(&omp, &opener, &provider, ctl_rx, st_tx);
        let drive = async {
            while let Some(s) = st_rx.recv().await {
                if matches!(s, LoginState::WaitingForInput { .. }) {
                    ctl_tx.send(LoginControl::SubmitInput("code#state".into())).unwrap();
                    break;
                }
            }
        };
        let (end, ()) = tokio::join!(run, drive);
        assert_eq!(
            end,
            LoginState::Succeeded {
                provider: "anthropic".into()
            }
        );
        assert_eq!(
            *omp.responses.lock().unwrap(),
            [UiResponse::Value {
                id: "2".into(),
                value: "code#state".into()
            }]
        );
    }

    #[tokio::test]
    async fn cancelling_while_omp_waits_for_input_withdraws_the_request() {
        let omp = FakeOmp::new(Script::Hang(vec![open_url(None), input()]));
        let opener = RecordingOpener::default();
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let (st_tx, mut st_rx) = mpsc::unbounded_channel();
        let provider = anthropic();
        let run = run_login(&omp, &opener, &provider, ctl_rx, st_tx);
        let drive = async {
            while let Some(s) = st_rx.recv().await {
                if matches!(s, LoginState::WaitingForInput { .. }) {
                    ctl_tx.send(LoginControl::Cancel).unwrap();
                    break;
                }
            }
        };
        let (end, ()) = tokio::join!(run, drive);
        assert_eq!(
            end,
            LoginState::Cancelled {
                provider: "anthropic".into()
            }
        );
        assert_eq!(
            *omp.responses.lock().unwrap(),
            [UiResponse::Cancelled {
                id: "2".into(),
                cancelled: true
            }]
        );
    }

    #[tokio::test]
    async fn browser_failure_keeps_the_copyable_launch_url() {
        let omp = FakeOmp::new(Script::Hang(vec![open_url(Some("http://127.0.0.1:41234/login"))]));
        let opener = RecordingOpener {
            fail: true,
            ..Default::default()
        };
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let (st_tx, mut st_rx) = mpsc::unbounded_channel();
        let provider = anthropic();
        let run = run_login(&omp, &opener, &provider, ctl_rx, st_tx);
        let drive = async {
            let mut shown = None;
            while let Some(s) = st_rx.recv().await {
                if let LoginState::WaitingForBrowser {
                    open_error: Some(_),
                    ref copy_url,
                    ..
                } = s
                {
                    shown = Some(copy_url.clone());
                    ctl_tx.send(LoginControl::Cancel).unwrap();
                    break;
                }
            }
            shown
        };
        let (_, shown) = tokio::join!(run, drive);
        assert_eq!(shown.as_deref(), Some("http://127.0.0.1:41234/login"));
    }

    #[tokio::test]
    async fn omp_refusal_is_reported_as_failure_with_its_message() {
        let omp = FakeOmp::new(Script::Frames(
            vec![],
            Err(RpcError::Command {
                command: "login".into(),
                message: "Provider 'x' requires secret input, which is not supported in RPC mode. Use the terminal UI to log in.".into(),
                code: None,
            }),
        ));
        let opener = RecordingOpener::default();
        let (_ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let (st_tx, _st_rx) = mpsc::unbounded_channel();
        let end = run_login(&omp, &opener, &anthropic(), ctl_rx, st_tx).await;
        assert!(matches!(end, LoginState::Failed { ref message, .. } if message.contains("terminal UI")));
    }

    #[test]
    fn second_login_while_one_runs_is_refused_and_paused_providers_cannot_start() {
        let mut flow = LoginFlow::new();
        flow.start(&anthropic()).unwrap();
        assert!(flow.start(&anthropic()).is_err());
        let mut flow = LoginFlow::new();
        let paused = LoginProvider {
            available: false,
            ..anthropic()
        };
        assert!(flow.start(&paused).is_err());
        assert_eq!(flow.state(), &LoginState::Idle);
    }

    #[test]
    fn late_result_after_cancel_does_not_resurrect_the_login() {
        let mut flow = LoginFlow::new();
        flow.start(&anthropic()).unwrap();
        flow.cancel();
        flow.on_finished(Ok("anthropic".into()));
        assert_eq!(
            flow.state(),
            &LoginState::Cancelled {
                provider: "anthropic".into()
            }
        );
    }
}
