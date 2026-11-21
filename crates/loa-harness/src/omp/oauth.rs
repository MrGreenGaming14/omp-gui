//! omp OAuth login strategy.
use std::sync::Arc;
use tokio::sync::mpsc;
use loa_contracts::harness::{LoginControl, LoginStrategy, LoginUpdate, UrlOpener};
use loa_settings::models::rpc::{LoginProvider, LoginUiRequest, OmpRpc, UiResponse};

/// Runs omp's OAuth protocol and publishes the same updates as CLI logins.
pub struct OmpOAuthLogin {
    pub rpc: Arc<dyn OmpRpc>,
    pub opener: Arc<dyn UrlOpener>,
    pub provider: LoginProvider,
}

#[async_trait::async_trait]
impl LoginStrategy for OmpOAuthLogin {
    async fn run(self: Box<Self>, mut controls: mpsc::Receiver<LoginControl>, updates: mpsc::Sender<LoginUpdate>) {
        if !self.provider.available {
            let _ = updates.send(LoginUpdate::Failed(format!("omp offers no login for {} now", self.provider.name))).await;
            return;
        }
        let (ui_tx, mut ui_rx) = mpsc::unbounded_channel();
        let login = self.rpc.login(&self.provider.id, ui_tx);
        let mut login = std::pin::pin!(login);
        let mut pending: Option<String> = None;
        let mut controls_open = true;
        loop {
            tokio::select! {
                biased;
                Some(request) = ui_rx.recv() => {
                    if !handle_ui(request, &*self.opener, &updates, &mut pending).await { return; }
                }
                control = controls.recv(), if controls_open => match control {
                    Some(LoginControl::SubmitInput(value)) => {
                        if let Some(id) = pending.take()
                            && let Err(err) = self.rpc.respond_ui(UiResponse::Value { id, value }).await
                        {
                            let _ = updates.send(LoginUpdate::Failed(err.to_string())).await;
                            return;
                        }
                    }
                    Some(LoginControl::Cancel) => {
                        if let Some(id) = pending.take() {
                            let _ = self.rpc.respond_ui(UiResponse::Cancelled { id, cancelled: true }).await;
                        }
                        let _ = updates.send(LoginUpdate::Cancelled).await;
                        return;
                    }
                    None => controls_open = false,
                },
                result = &mut login => {
                    while let Ok(request) = ui_rx.try_recv() {
                        if !handle_ui(request, &*self.opener, &updates, &mut pending).await { return; }
                    }
                    let update = match result {
                        Ok(_) => LoginUpdate::Succeeded { account: None },
                        Err(err) => LoginUpdate::Failed(err.to_string()),
                    };
                    let _ = updates.send(update).await;
                    return;
                }
            }
        }
    }
}

async fn handle_ui(request: LoginUiRequest, opener: &dyn UrlOpener, updates: &mpsc::Sender<LoginUpdate>, pending: &mut Option<String>) -> bool {
    let update = match request {
        LoginUiRequest::OpenUrl { url, launch_url, instructions, .. } => {
            let error = opener.open_url(&url).err();
            if updates.send(LoginUpdate::Url { url, launch_url, instructions }).await.is_err() { return false; }
            error.map(|message| LoginUpdate::Progress(format!("Could not open browser: {message}")))
        }
        LoginUiRequest::Notify { message, .. } => Some(LoginUpdate::Progress(message)),
        LoginUiRequest::Input { id, title, placeholder, .. } => {
            *pending = Some(id);
            Some(LoginUpdate::Prompt { message: title, placeholder })
        }
        LoginUiRequest::Cancel { target_id, .. } => {
            if pending.as_ref() == Some(&target_id) { *pending = None; }
            None
        }
    };
    match update { Some(update) => updates.send(update).await.is_ok(), None => true }
}

#[cfg(test)]
mod tests {
    use super::*;
    use loa_models::testing::{FakeOmp, LoginScript};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Opener(Mutex<Vec<String>>);
    impl UrlOpener for Opener {
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.0.lock().unwrap().push(url.to_owned());
            Ok(())
        }
    }
    fn provider() -> LoginProvider {
        LoginProvider { id: "anthropic".into(), name: "Anthropic".into(), available: true, authenticated: false }
    }
    fn frames() -> Vec<LoginUiRequest> {
        vec![
            LoginUiRequest::OpenUrl { id: "1".into(), url: "https://auth.example".into(), launch_url: Some("http://localhost/login".into()), instructions: None },
            LoginUiRequest::Input { id: "2".into(), title: "Paste code".into(), placeholder: None, timeout: None },
        ]
    }
    #[tokio::test]
    async fn relays_oauth_input_and_success() {
        let mut fake = FakeOmp::default();
        fake.login_frames = frames();
        fake.script = LoginScript::AwaitInput("code".into());
        let rpc = Arc::new(fake);
        let opener = Arc::new(Opener::default());
        let (ctl_tx, ctl_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);
        let strategy = Box::new(OmpOAuthLogin { rpc: rpc.clone(), opener: opener.clone(), provider: provider() });
        let drive = async {
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Url { url: "https://auth.example".into(), launch_url: Some("http://localhost/login".into()), instructions: None }));
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Prompt { .. })));
            ctl_tx.send(LoginControl::SubmitInput("code".into())).await.unwrap();
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Succeeded { account: None }));
        };
        tokio::join!(strategy.run(ctl_rx, out_tx), drive);
        assert_eq!(*opener.0.lock().unwrap(), ["https://auth.example"]);
        assert_eq!(*rpc.responses.lock().unwrap(), [UiResponse::Value { id: "2".into(), value: "code".into() }]);
    }
    #[tokio::test]
    async fn cancellation_withdraws_pending_input() {
        let mut fake = FakeOmp::default();
        fake.login_frames = frames();
        fake.script = LoginScript::Hang;
        let rpc = Arc::new(fake);
        let (ctl_tx, ctl_rx) = mpsc::channel(16);
        let (out_tx, mut out_rx) = mpsc::channel(16);
        let strategy = Box::new(OmpOAuthLogin { rpc: rpc.clone(), opener: Arc::new(Opener::default()), provider: provider() });
        let drive = async {
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Url { .. })));
            assert!(matches!(out_rx.recv().await, Some(LoginUpdate::Prompt { .. })));
            ctl_tx.send(LoginControl::Cancel).await.unwrap();
            assert_eq!(out_rx.recv().await, Some(LoginUpdate::Cancelled));
        };
        tokio::join!(strategy.run(ctl_rx, out_tx), drive);
        assert_eq!(*rpc.responses.lock().unwrap(), [UiResponse::Cancelled { id: "2".into(), cancelled: true }]);
    }
}
