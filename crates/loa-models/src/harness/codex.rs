//! Codex CLI sign in and its user-level `config.toml` model settings.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;

use super::{HarnessAuth, HarnessId, LoginKind, SignInMethod, SignInState, validate_model_name};
use super::configdoc::{ConfigDoc, TomlDoc, edit_file};
use crate::error::load_snapshot;
use crate::process::{CommandSpec, ProcessRunner, RunningProcess};
use crate::{Error, Result};

/// Codex CLI, its user config path and a process runner. Paths are supplied by
/// the app (normally `codex` and `~/.codex/config.toml`).
#[derive(Clone)]
pub struct Codex {
    pub program: PathBuf,
    pub config_path: PathBuf,
    pub runner: Arc<dyn ProcessRunner>,
}

loa_util::str_enum! {
    /// The supported Codex reasoning effort values in `config.toml`.
    pub ReasoningEffort {
        None => "none",
        Minimal => "minimal",
        Low => "low",
        Medium => "medium",
        High => "high",
        XHigh => "xhigh",
    }
}

/// Codex's persisted model and reasoning effort, without other config keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexModelSettings {
    pub model: Option<String>,
    pub reasoning_effort: Option<ReasoningEffort>,
}

impl Codex {
    pub fn new(program: impl Into<PathBuf>, config_path: impl Into<PathBuf>, runner: Arc<dyn ProcessRunner>) -> Self {
        Self { program: program.into(), config_path: config_path.into(), runner }
    }
    fn command(&self, args: &[&str]) -> CommandSpec {
        CommandSpec::new(&self.program, args.iter().copied())
    }
    fn load(&self) -> Result<TomlDoc> {
        let (_, text) = load_snapshot(&self.config_path)?;
        TomlDoc::parse(text.as_deref().unwrap_or(""), &self.config_path.display().to_string())
    }
    /// Reads only model fields; a missing file has no overrides.
    pub fn read_model_settings(&self) -> Result<CodexModelSettings> {
        let doc = self.load()?;
        Ok(CodexModelSettings {
            model: doc.get(&["model"]),
            reasoning_effort: doc.get(&["model_reasoning_effort"]).as_deref().and_then(ReasoningEffort::parse),
        })
    }
    /// Changes the default model, keeping comments and every other TOML key.
    pub fn set_model(&self, model: Option<&str>) -> Result<()> {
        if let Some(name) = model { validate_model_name("model", name)?; }
        self.edit("model", model)
    }
    /// Changes Codex's `model_reasoning_effort` (not omp's thinking level).
    pub fn set_reasoning_effort(&self, effort: Option<ReasoningEffort>) -> Result<()> {
        self.edit("model_reasoning_effort", effort.map(ReasoningEffort::as_str))
    }
    fn edit(&self, key: &str, setting: Option<&str>) -> Result<()> {
        let (mut snapshot, text) = load_snapshot(&self.config_path)?;
        edit_file::<TomlDoc>(&mut snapshot, text.as_deref().unwrap_or(""), |doc| doc.set(&[key], setting))?;
        Ok(())
    }
}

fn status(code: Option<i32>, text: &str) -> SignInState {
    match code {
        Some(1) => SignInState::SignedOut,
        Some(0) => {
            let lower = text.to_ascii_lowercase();
            let method = if lower.contains("chatgpt") { SignInMethod::Subscription }
                else if lower.contains("api key") { SignInMethod::ApiKey }
                else { SignInMethod::Other("unknown".into()) };
            SignInState::SignedIn { method }
        }
        _ => SignInState::Unknown { reason: "codex login status did not complete successfully".into() },
    }
}

#[async_trait]
impl HarnessAuth for Codex {
    fn id(&self) -> HarnessId { HarnessId::Codex }
    async fn sign_in_state(&self) -> Result<SignInState> {
        match self.runner.output(&self.command(&["login", "status"])).await {
            Ok(out) => Ok(status(out.code, &out.stdout)),
            Err(Error::Process { .. }) => Ok(SignInState::Unknown { reason: "codex login status could not run".into() }),
            Err(e) => Err(e),
        }
    }
    fn login_command(&self, kind: LoginKind) -> Result<CommandSpec> {
        match kind {
            LoginKind::Browser => Ok(self.command(&["login"])),
            LoginKind::DeviceCode => Ok(self.command(&["login", "--device-auth"])),
            _ => Err(Error::invalid("login kind", "Codex supports browser and device-code login")),
        }
    }
    async fn start_login(&self, kind: LoginKind) -> Result<Box<dyn RunningProcess>> {
        self.runner.spawn(&self.login_command(kind)?).await
    }
    fn logout_command(&self) -> CommandSpec { self.command(&["logout"]) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::TokioProcessRunner;
    use crate::process::testutil::fake_program;
    #[tokio::test]
    async fn login_status_and_device_flow_use_codex_cli() {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_program(dir.path(), "codex", "if [ \"$1 $2\" = 'login status' ]; then echo 'Logged in using ChatGPT'; exit 0; fi\nif [ \"$1 $2\" = 'login --device-auth' ]; then exit 7; fi\nexit 64");
        let codex = Codex::new(program, dir.path().join("config.toml"), Arc::new(TokioProcessRunner));
        assert_eq!(codex.sign_in_state().await.unwrap(), SignInState::SignedIn { method: SignInMethod::Subscription });
        let mut login = codex.start_login(LoginKind::DeviceCode).await.unwrap();
        assert_eq!(login.wait().await.unwrap(), Some(7));
    }
    #[test]
    fn model_edits_preserve_other_fields_and_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "# keep this\nmodel = 'old' # user's note\nmodel_reasoning_effort = 'medium'\n[projects.'/work']\ntrust_level = 'trusted'\n").unwrap();
        let codex = Codex::new("codex", &path, Arc::new(TokioProcessRunner));
        codex.set_model(Some("gpt-5.4")).unwrap();
        codex.set_reasoning_effort(Some(ReasoningEffort::High)).unwrap();
        assert_eq!(codex.read_model_settings().unwrap(), CodexModelSettings { model: Some("gpt-5.4".into()), reasoning_effort: Some(ReasoningEffort::High) });
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("# keep this"));
        assert!(contents.contains("[projects.'/work']"));
        assert!(contents.contains("# user's note"));
    }
}
