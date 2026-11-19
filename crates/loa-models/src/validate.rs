//! Validates candidate omp files in an isolated temporary agent directory.
//! Only read-only, no-prompt/no-model-call commands are invoked.

use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

use crate::process::{CommandSpec, ProcessRunner};
use crate::{Error, Result};

/// A validator for candidate config and models files, used before atomic saves.
#[async_trait]
pub trait SettingsValidator: Send + Sync {
    /// Confirms omp reads each touched setting as the proposed value.
    async fn check_config(&self, text: &str, expected: &[(String, serde_json::Value)]) -> Result<()>;
    /// Confirms omp accepts the candidate model document.
    async fn check_models(&self, text: &str) -> Result<()>;
}

/// Calls omp CLI with its agent directory redirected to a temp directory.
/// It never sends a prompt; catalog listing may still discover local models.
#[derive(Clone)]
pub struct OmpSettingsValidator {
    pub program: PathBuf,
    pub runner: Arc<dyn ProcessRunner>,
}

impl OmpSettingsValidator {
    pub fn new(program: impl Into<PathBuf>, runner: Arc<dyn ProcessRunner>) -> Self {
        Self { program: program.into(), runner }
    }

    async fn command(&self, text: &str, file: &str, args: &[&str]) -> Result<String> {
        let dir = tempfile::tempdir().map_err(|e| Error::io("temporary omp config", e))?;
        let path = dir.path().join(file);
        std::fs::write(&path, text).map_err(|e| Error::io(&path, e))?;
        let mut spec = CommandSpec::new(&self.program, args.iter().copied());
        spec.env.push(("PI_CODING_AGENT_DIR".into(), dir.path().to_string_lossy().into_owned()));
        spec.env.push(("OMP_PROFILE".into(), "default".into()));
        spec.env.push(("PI_CONFIG_FILES".into(), "".into()));
        spec.current_dir = Some(dir.path().to_path_buf());
        let out = self.runner.output(&spec).await?;
        if out.code != Some(0) {
            return Err(Error::Rejected { what: file.into(), message: out.stderr.trim().to_owned() });
        }
        if file == "models.yml" && out.stderr.contains("models.yml") {
            return Err(Error::Rejected { what: file.into(), message: out.stderr.trim().to_owned() });
        }
        Ok(out.stdout)
    }
}

#[async_trait]
impl SettingsValidator for OmpSettingsValidator {
    async fn check_config(&self, text: &str, expected: &[(String, serde_json::Value)]) -> Result<()> {
        // Scope global returns exactly the written layer, not merged defaults.
        let stdout = self.command(text, "config.yml", &["config", "list", "--scope", "global", "--json"]).await?;
        let values: serde_json::Value = serde_json::from_str(&stdout).map_err(|e| Error::Parse { what: "omp config list --json".into(), message: e.to_string() })?;
        for (key, value) in expected {
            let actual = values.get(key).and_then(|entry| entry.get("value")).cloned().unwrap_or(serde_json::Value::Null);
            if actual != *value {
                return Err(Error::Rejected { what: "config.yml".into(), message: format!("omp did not accept setting {key}") });
            }
        }
        Ok(())
    }

    async fn check_models(&self, text: &str) -> Result<()> {
        // models ls loads the model registry without asking a model anything.
        self.command(text, "models.yml", &["models", "ls", "--json", "--no-extensions"]).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{ProcessOutput, RunningProcess};
    use std::sync::Mutex;
    struct Runner(Mutex<Vec<CommandSpec>>);
    #[async_trait]
    impl ProcessRunner for Runner {
        async fn output(&self, cmd: &CommandSpec) -> Result<ProcessOutput> {
            self.0.lock().unwrap().push(cmd.clone());
            let text = std::fs::read_to_string(cmd.current_dir.as_ref().unwrap().join("config.yml")).unwrap_or_default();
            Ok(ProcessOutput { code: Some(0), stdout: if text.contains("modelRoles:") { "{\"modelRoles\":{\"value\":{\"smol\":\"openai/gpt\"}}}".into() } else { "[]".into() }, stderr: String::new() })
        }
        async fn spawn(&self, _: &CommandSpec) -> Result<Box<dyn RunningProcess>> { unreachable!() }
    }
    #[tokio::test]
    async fn validates_written_role_without_loading_real_agent_directory() {
        let runner = Arc::new(Runner(Mutex::new(vec![])));
        let validator = OmpSettingsValidator::new("omp", runner.clone());
        validator.check_config("modelRoles:\n  smol: openai/gpt\n", &[("modelRoles".into(), serde_json::json!({"smol":"openai/gpt"}))]).await.unwrap();
        let calls = runner.0.lock().unwrap();
        assert_eq!(calls[0].args, ["config", "list", "--scope", "global", "--json"]);
        assert!(calls[0].current_dir.is_some());
    }
}
