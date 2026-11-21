//! omp OAuth login strategy.
use std::sync::Arc;
use tokio::sync::mpsc;
use loa_contracts::harness::{LoginControl, LoginStrategy, LoginUpdate, UrlOpener};
use loa_contracts::harness::model_rpc::{LoginProvider, LoginUiRequest, OmpRpc, UiResponse};

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

