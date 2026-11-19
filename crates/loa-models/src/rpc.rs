//! The omp RPC port: the commands this crate needs, shaped after omp's RPC
//! protocol (`omp --mode rpc`, see omp's `rpc.md`) so a real client implements
//! it by sending one command and decoding its `data`.
//!
//! | Method | RPC command | Response `data` |
//! | --- | --- | --- |
//! | [`OmpRpc::get_login_providers`] | `{"type":"get_login_providers"}` | `{ providers: LoginProvider[] }` |
//! | [`OmpRpc::login`] | `{"type":"login","providerId"}` | `{ providerId }`, with `extension_ui_request` frames (`open_url`, `notify`, `input`, `cancel`) while it runs |
//! | [`OmpRpc::respond_ui`] | `{"type":"extension_ui_response",...}` | none |
//! | [`OmpRpc::get_available_models`] | `{"type":"get_available_models"}` | `{ models: ModelInfo[] }` |
//! | [`OmpRpc::set_model`] | `{"type":"set_model","provider","modelId"}` | `ModelInfo` |
//!
//! omp settings cannot be changed over RPC: the protocol has no command that
//! writes `config.yml` or `models.yml` (session-scoped toggles such as
//! `set_steering_mode` or `set_cache_warming` never persist). Long-lived RPC
//! processes watch the files and reload them, so this crate edits the files
//! (see [`crate::config`] and [`crate::models_yml`]).

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

/// A failed RPC call: omp's `success: false` response or a transport failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RpcError {
    /// omp answered `success: false` for the command.
    #[error("omp {command} failed: {message}")]
    Command {
        command: String,
        message: String,
        code: Option<String>,
    },
    /// The omp process or its stdio is gone.
    #[error("omp RPC connection failed: {0}")]
    Transport(String),
}

/// An entry of `get_login_providers`: an OAuth or login-capable provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginProvider {
    pub id: String,
    pub name: String,
    /// Whether omp offers a login for it now (paused providers are not).
    pub available: bool,
    /// Whether omp holds a credential for it.
    pub authenticated: bool,
}

/// Per million token USD rates of a model.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// A model as `get_available_models` and `set_model` return it (omp's
/// `ModelInfo`). Fields the picker does not use are kept in `extra`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelInfo {
    pub id: String,
    pub name: String,
    pub provider: String,
    #[serde(default)]
    pub api: String,
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub input: Vec<String>,
    #[serde(default)]
    pub cost: ModelCost,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_tokens: Option<u64>,
    #[serde(default)]
    pub thinking: Option<ThinkingConfig>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

impl ModelInfo {
    /// The exact `provider/modelId` selector omp accepts everywhere.
    pub fn selector(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }
}

/// A model's thinking support as omp reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThinkingConfig {
    pub mode: String,
    #[serde(default)]
    pub efforts: Vec<String>,
    #[serde(default)]
    pub default_level: Option<String>,
}

/// A UI frame omp sends while a `login` runs (`extension_ui_request`). Other
/// methods do not occur during a host-issued login over `--mode rpc`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum LoginUiRequest {
    /// The authorization URL to open in the browser. `launch_url`, when present,
    /// is a short loopback redirect to it, the safer text to copy.
    #[serde(rename_all = "camelCase")]
    OpenUrl {
        id: String,
        url: String,
        #[serde(default)]
        launch_url: Option<String>,
        #[serde(default)]
        instructions: Option<String>,
    },
    /// Progress text from the provider's login flow.
    #[serde(rename_all = "camelCase")]
    Notify {
        id: String,
        message: String,
        #[serde(default)]
        notify_type: Option<String>,
    },
    /// A non-secret value the user must type (for example a pasted callback
    /// URL). omp rejects secret prompts over RPC before sending them.
    Input {
        id: String,
        title: String,
        #[serde(default)]
        placeholder: Option<String>,
        #[serde(default)]
        timeout: Option<u64>,
    },
    /// omp withdrew an earlier request (timeout or abort).
    #[serde(rename_all = "camelCase")]
    Cancel { id: String, target_id: String },
}

/// The host's answer to an `input` request (`extension_ui_response`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UiResponse {
    Value { id: String, value: String },
    Cancelled { id: String, cancelled: bool },
}

impl UiResponse {
    /// The request this response answers.
    pub fn id(&self) -> &str {
        match self {
            Self::Value { id, .. } | Self::Cancelled { id, .. } => id,
        }
    }
}

/// The omp RPC commands this crate uses. The app backs it with its omp RPC
/// client; tests back it with an in-memory fake.
#[async_trait::async_trait]
pub trait OmpRpc: Send + Sync {
    /// `get_login_providers`.
    async fn get_login_providers(&self) -> Result<Vec<LoginProvider>, RpcError>;

    /// `login` for `provider_id`. Every `extension_ui_request` frame omp emits
    /// while the command runs is forwarded to `ui`. Resolves with the response:
    /// the provider id on success.
    async fn login(
        &self,
        provider_id: &str,
        ui: UnboundedSender<LoginUiRequest>,
    ) -> Result<String, RpcError>;

    /// Sends an `extension_ui_response`.
    async fn respond_ui(&self, response: UiResponse) -> Result<(), RpcError>;

    /// `get_available_models`.
    async fn get_available_models(&self) -> Result<Vec<ModelInfo>, RpcError>;

    /// `set_model`: switches the session's model; returns the selected model.
    async fn set_model(&self, provider: &str, model_id: &str) -> Result<ModelInfo, RpcError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_omp_login_ui_frames() {
        let open: LoginUiRequest = serde_json::from_str(
            r#"{"type":"extension_ui_request","id":"7","method":"open_url","url":"https://auth.example/authorize?x=1","launchUrl":"http://127.0.0.1:5555/l","instructions":"Sign in"}"#,
        )
        .unwrap();
        assert_eq!(
            open,
            LoginUiRequest::OpenUrl {
                id: "7".into(),
                url: "https://auth.example/authorize?x=1".into(),
                launch_url: Some("http://127.0.0.1:5555/l".into()),
                instructions: Some("Sign in".into()),
            }
        );
        let input: LoginUiRequest = serde_json::from_str(
            r#"{"type":"extension_ui_request","id":"8","method":"input","title":"Paste the callback URL","timeout":600000}"#,
        )
        .unwrap();
        assert!(matches!(input, LoginUiRequest::Input { ref title, .. } if title == "Paste the callback URL"));
        let notify: LoginUiRequest = serde_json::from_str(
            r#"{"type":"extension_ui_request","id":"9","method":"notify","message":"Waiting for browser","notifyType":"info"}"#,
        )
        .unwrap();
        assert!(matches!(notify, LoginUiRequest::Notify { ref message, .. } if message == "Waiting for browser"));
    }

    #[test]
    fn encodes_ui_responses_as_omp_expects() {
        let v = serde_json::to_value(UiResponse::Value {
            id: "8".into(),
            value: "http://cb".into(),
        })
        .unwrap();
        assert_eq!(v, serde_json::json!({"id":"8","value":"http://cb"}));
        let c = serde_json::to_value(UiResponse::Cancelled {
            id: "8".into(),
            cancelled: true,
        })
        .unwrap();
        assert_eq!(c, serde_json::json!({"id":"8","cancelled":true}));
    }

    #[test]
    fn decodes_model_info_with_null_limits_and_unknown_fields() {
        let m: ModelInfo = serde_json::from_str(
            r#"{"id":"m1","name":"M One","api":"openai-completions","provider":"gw","baseUrl":"http://x","reasoning":false,"input":["text"],"cost":{"input":1,"output":2,"cacheRead":0,"cacheWrite":0},"contextWindow":null,"maxTokens":4096,"priority":3}"#,
        )
        .unwrap();
        assert_eq!(m.selector(), "gw/m1");
        assert_eq!(m.context_window, None);
        assert_eq!(m.max_tokens, Some(4096));
        assert_eq!(m.extra.get("priority"), Some(&serde_json::json!(3)));
    }
}
