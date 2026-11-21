//! Claude Code: sign in through `claude auth` and model settings in its user
//! `settings.json`.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};

use super::configdoc::{ConfigDoc, JsonDoc, edit_file};
use super::{HarnessAuth, HarnessId, LoginKind, SignInMethod, SignInState, validate_model_name};
use crate::error::{load_snapshot, read_optional};
use crate::process::{CommandSpec, ProcessRunner, RunningProcess};
use crate::{Error, Result};

/// Key of the default model in `settings.json`.
const MODEL_KEY: &str = "model";
/// Key of the default effort level, at the top level and in a model's entry.
const EFFORT_KEY: &str = "effortLevel";
/// Key of the per model settings object.
const MODEL_SETTINGS_KEY: &str = "modelSettings";
/// Model value Claude Code treats as "no override".
const DEFAULT_MODEL: &str = "default";

/// Claude Code reached through its CLI at `program`, with its user settings
/// file at `settings_path` (normally `~/.claude/settings.json`).
#[derive(Clone)]
pub struct ClaudeCode {
    /// The `claude` executable.
    pub program: PathBuf,
    /// The user `settings.json`.
    pub settings_path: PathBuf,
    /// Runs the CLI.
    pub runner: Arc<dyn ProcessRunner>,
}

/// An effort level Claude Code accepts in `settings.json`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClaudeEffort { Low, Medium, High, XHigh }

impl ClaudeEffort {
    /// Every level, lowest first.
    pub const ALL: [Self; 4] = [Self::Low, Self::Medium, Self::High, Self::XHigh];

    /// The value written to `settings.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
        }
    }

    /// Parses a `settings.json` value; anything else is [`Error::Invalid`].
    pub fn parse(text: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|level| level.as_str() == text)
            .ok_or_else(|| {
                Error::invalid(
                    EFFORT_KEY,
                    format!("{text:?} is not one of low, medium, high, xhigh"),
                )
            })
    }
}

/// The model settings Claude Code reads from `settings.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClaudeModelSettings {
    /// Default model (alias or full id); `None` when Claude Code picks.
    pub model: Option<String>,
    /// Default effort level; `None` when the model's own default applies.
    pub effort: Option<ClaudeEffort>,
    /// Effort level per model name, from `modelSettings.<model>.effortLevel`.
    pub per_model_effort: BTreeMap<String, ClaudeEffort>,
}

impl ClaudeCode {
    /// Claude Code with its CLI at `program` and settings at `settings_path`.
    pub fn new(
        program: impl Into<PathBuf>,
        settings_path: impl Into<PathBuf>,
        runner: Arc<dyn ProcessRunner>,
    ) -> Self {
        Self {
            program: program.into(),
            settings_path: settings_path.into(),
            runner,
        }
    }

    fn command(&self, args: &[&str]) -> CommandSpec {
        CommandSpec::new(&self.program, args.iter().copied())
    }

    fn what(&self) -> String {
        self.settings_path.display().to_string()
    }

    /// Reads the model settings. A missing file has none. Values Claude Code
    /// would not accept (a non-string model, an unknown effort level) are
    /// reported as unset, as Claude Code ignores them too.
    pub fn read_model_settings(&self) -> Result<ClaudeModelSettings> {
        let text = read_optional(&self.settings_path)?.unwrap_or_default();
        let root = JsonDoc::parse(&text, &self.what())?.root;
        let model = root
            .get(MODEL_KEY)
            .and_then(Value::as_str)
            .filter(|m| *m != DEFAULT_MODEL)
            .map(str::to_owned);
        let per_model_effort = root
            .get(MODEL_SETTINGS_KEY)
            .and_then(Value::as_object)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|(name, entry)| Some((name.clone(), effort_of(entry)?)))
                    .collect()
            })
            .unwrap_or_default();
        Ok(ClaudeModelSettings {
            model,
            effort: effort_of(&Value::Object(root)),
            per_model_effort,
        })
    }

    /// Sets the default model, or removes it for `None` and `default`.
    pub fn set_model(&self, model: Option<&str>) -> Result<()> {
        let model = match model {
            None | Some(DEFAULT_MODEL) => None,
            Some(name) => {
                validate_model_name(MODEL_KEY, name)?;
                Some(name)
            }
        };
        self.edit(|doc| doc.set(&[MODEL_KEY], model))
    }

    /// Sets the default effort level, or removes it for `None`.
    pub fn set_effort(&self, effort: Option<ClaudeEffort>) -> Result<()> {
        self.edit(|doc| doc.set(&[EFFORT_KEY], effort.map(ClaudeEffort::as_str)))
    }

    /// Sets `modelSettings.<model>.effortLevel`, keeping the entry's other
    /// fields and the other models. Removing the last field of an entry removes
    /// the entry; removing the last entry removes `modelSettings`.
    pub fn set_model_effort(&self, model: &str, effort: Option<ClaudeEffort>) -> Result<()> {
        validate_model_name("modelSettings model", model)?;
        self.edit(|doc| {
            let path: &[&str] = if effort.is_none() {
                let models = doc.root.get(MODEL_SETTINGS_KEY).and_then(Value::as_object);
                let entry = models.and_then(|m| m.get(model)).and_then(Value::as_object);
                match (models, entry) {
                    (Some(models), Some(entry)) if only_key(entry, EFFORT_KEY) => {
                        if only_key(models, model) {
                            &[MODEL_SETTINGS_KEY]
                        } else {
                            &[MODEL_SETTINGS_KEY, model]
                        }
                    }
                    _ => &[MODEL_SETTINGS_KEY, model, EFFORT_KEY],
                }
            } else {
                &[MODEL_SETTINGS_KEY, model, EFFORT_KEY]
            };
            doc.set(path, effort.map(ClaudeEffort::as_str))
        })
    }

    /// Applies an edit and saves it atomically if the document changed.
    fn edit(&self, edit: impl FnOnce(&mut JsonDoc) -> Result<()>) -> Result<()> {
        let (mut snapshot, text) = load_snapshot(&self.settings_path)?;
        edit_file(&mut snapshot, text.as_deref().unwrap_or(""), edit)?;
        Ok(())
    }
}

/// The `effortLevel` of a settings object, when it is one Claude Code accepts.
fn effort_of(value: &Value) -> Option<ClaudeEffort> {
    value
        .get(EFFORT_KEY)
        .and_then(Value::as_str)
        .and_then(|text| ClaudeEffort::parse(text).ok())
}

/// Whether `map` holds exactly the one key `key`.
fn only_key(map: &Map<String, Value>, key: &str) -> bool {
    map.len() == 1 && map.contains_key(key)
}

/// The parts of `claude auth status` JSON this crate reads.
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    auth_method: Option<String>,
    logged_in: Option<bool>,
}

/// Maps the output of `claude auth status` to a sign in state: exit 1 is signed
/// out, exit 0 with JSON is mapped by `authMethod`, anything else is unknown.
fn parse_auth_status(code: Option<i32>, stdout: &str) -> SignInState {
    match code {
        Some(0) => {}
        Some(1) => return SignInState::SignedOut,
        Some(other) => {
            return SignInState::Unknown {
                reason: format!("claude auth status exited with code {other}"),
            };
        }
        None => {
            return SignInState::Unknown {
                reason: "claude auth status was ended by a signal".to_owned(),
            };
        }
    }
    let status: AuthStatus = match serde_json::from_str(stdout.trim()) {
        Ok(status) => status,
        Err(err) => {
            return SignInState::Unknown {
                reason: format!("claude auth status printed unexpected output: {err}"),
            };
        }
    };
    if status.logged_in == Some(false) {
        return SignInState::SignedOut;
    }
    let method = match status.auth_method.as_deref() {
        Some("none") => return SignInState::SignedOut,
        Some("claude.ai") => SignInMethod::Subscription,
        Some("api_key") => SignInMethod::ApiKey,
        Some("api_key_helper") => SignInMethod::ApiKeyHelper,
        Some("oauth_token") => SignInMethod::OAuthToken,
        Some("third_party") => SignInMethod::ThirdParty,
        Some(name) if is_method_name(name) => SignInMethod::Other(name.to_owned()),
        _ => SignInMethod::Other("unknown".to_owned()),
    };
    SignInState::SignedIn { method }
}

/// Whether `name` looks like an auth method identifier (short, no spaces), so
/// it is safe to keep and show.
fn is_method_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 32
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}

#[async_trait]
impl HarnessAuth for ClaudeCode {
    fn id(&self) -> HarnessId {
        HarnessId::ClaudeCode
    }

    /// Runs `claude auth status`. A CLI that cannot be run is reported as
    /// [`SignInState::Unknown`], not as an error.
    async fn sign_in_state(&self) -> Result<SignInState> {
        let spec = self.command(&["auth", "status"]);
        match self.runner.output(&spec).await {
            Ok(out) => Ok(parse_auth_status(out.code, &out.stdout)),
            Err(Error::Process { message, .. }) => Ok(SignInState::Unknown {
                reason: format!("cannot run {}: {message}", spec.program_name()),
            }),
            Err(err) => Err(err),
        }
    }

    fn login_command(&self, kind: LoginKind) -> Result<CommandSpec> {
        match kind {
            LoginKind::Browser => Ok(self.command(&["auth", "login"])),
            LoginKind::Console => Ok(self.command(&["auth", "login", "--console"])),
            LoginKind::Sso => Ok(self.command(&["auth", "login", "--sso"])),
            LoginKind::DeviceCode => Err(Error::invalid(
                "login kind",
                "Claude Code has no device code login",
            )),
        }
    }

    async fn start_login(&self, kind: LoginKind) -> Result<Box<dyn RunningProcess>> {
        let spec = self.login_command(kind)?;
        self.runner.spawn(&spec).await
    }

    fn logout_command(&self) -> CommandSpec {
        self.command(&["auth", "logout"])
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::process::TokioProcessRunner;
    use crate::process::testutil::fake_program;

    fn claude(program: &Path, settings: &Path) -> ClaudeCode {
        ClaudeCode::new(program, settings, Arc::new(TokioProcessRunner))
    }

    /// A fake `claude` whose `auth status` prints `json` and exits with `code`.
    fn fake_status(dir: &Path, json: &str, code: i32) -> PathBuf {
        let body = format!(
            "if [ \"$1 $2\" = \"auth status\" ]; then\ncat <<'EOF'\n{json}\nEOF\nexit {code}\nfi\nexit 64"
        );
        fake_program(dir, "claude", &body)
    }

    async fn state_for(json: &str, code: i32) -> SignInState {
        let dir = tempfile::tempdir().unwrap();
        let program = fake_status(dir.path(), json, code);
        claude(&program, &dir.path().join("settings.json"))
            .sign_in_state()
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn signed_in_with_claude_ai_is_a_subscription() {
        let json = r#"{"loggedIn": true, "authMethod": "claude.ai", "email": "a@example.com",
            "orgName": "Org", "subscriptionType": "max", "loginExpiresAt": "2027-01-01T00:00:00Z",
            "somethingNew": {"x": 1}}"#;
        assert_eq!(
            state_for(json, 0).await,
            SignInState::SignedIn {
                method: SignInMethod::Subscription
            }
        );
    }

    #[tokio::test]
    async fn auth_methods_map_to_sign_in_methods() {
        let cases = [
            ("api_key", SignInMethod::ApiKey),
            ("api_key_helper", SignInMethod::ApiKeyHelper),
            ("oauth_token", SignInMethod::OAuthToken),
            ("third_party", SignInMethod::ThirdParty),
            ("future_method", SignInMethod::Other("future_method".into())),
        ];
        for (name, method) in cases {
            let json = format!(r#"{{"authMethod": "{name}"}}"#);
            assert_eq!(state_for(&json, 0).await, SignInState::SignedIn { method });
        }
    }

    #[tokio::test]
    async fn exit_one_or_method_none_is_signed_out() {
        let json = r#"{"loggedIn": false, "authMethod": "none"}"#;
        assert_eq!(state_for(json, 1).await, SignInState::SignedOut);
        assert_eq!(state_for(json, 0).await, SignInState::SignedOut);
    }

    #[tokio::test]
    async fn unparsable_output_and_missing_program_are_unknown() {
        assert!(matches!(
            state_for("Logged in as someone", 0).await,
            SignInState::Unknown { .. }
        ));
        assert!(matches!(
            state_for("{}", 2).await,
            SignInState::Unknown { .. }
        ));
        let dir = tempfile::tempdir().unwrap();
        let missing = claude(&dir.path().join("claude"), &dir.path().join("s.json"));
        assert!(matches!(
            missing.sign_in_state().await.unwrap(),
            SignInState::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn start_login_runs_claude_auth_login_with_kind_flags() {
        let dir = tempfile::tempdir().unwrap();
        let argv = dir.path().join("argv");
        let body = format!("printf '%s\\n' \"$@\" > '{}'\nexit 7", argv.display());
        let program = fake_program(dir.path(), "claude", &body);
        let harness = claude(&program, &dir.path().join("settings.json"));
        for (kind, expected) in [
            (LoginKind::Browser, "auth\nlogin\n"),
            (LoginKind::Console, "auth\nlogin\n--console\n"),
            (LoginKind::Sso, "auth\nlogin\n--sso\n"),
        ] {
            let mut proc = harness.start_login(kind).await.unwrap();
            assert_eq!(proc.wait().await.unwrap(), Some(7));
            assert_eq!(std::fs::read_to_string(&argv).unwrap(), expected);
        }
        let err = harness.start_login(LoginKind::DeviceCode).await.err().unwrap();
        assert!(matches!(err, Error::Invalid { .. }), "{err:?}");
        assert_eq!(harness.logout_command().args, ["auth", "logout"]);
    }

    const SETTINGS: &str = r#"{
    "$schema": "https://json.schemastore.org/claude-code-settings.json",
    "permissions": {
        "allow": ["Bash(npm run test:*)", "Read(~/.zshrc)"],
        "deny": ["Read(./.env)", "Bash(curl:*)"]
    },
    "model": "sonnet",
    "hooks": {
        "PostToolUse": [
            {"matcher": "Edit|Write", "hooks": [{"type": "command", "command": "jq -r '.tool_input.file_path' | { read f; prettier --write \"$f\"; }"}]}
        ]
    },
    "env": {"CLAUDE_CODE_ENABLE_TELEMETRY": "0", "Z": "}\"{"},
    "modelSettings": {
        "opus": {"maxEffortLevel": "max", "autoCompactWindow": 400000},
        "haiku": {
            "effortLevel": "low"
        }
    },
    "includeCoAuthoredBy": false
}
"#;

    fn settings_file(dir: &Path) -> PathBuf {
        let path = dir.join("settings.json");
        std::fs::write(&path, SETTINGS).unwrap();
        path
    }

    #[test]
    fn reads_model_settings() {
        let dir = tempfile::tempdir().unwrap();
        let harness = claude(Path::new("claude"), &settings_file(dir.path()));
        let settings = harness.read_model_settings().unwrap();
        assert_eq!(settings.model.as_deref(), Some("sonnet"));
        assert_eq!(settings.effort, None);
        assert_eq!(
            settings.per_model_effort,
            BTreeMap::from([("haiku".to_owned(), ClaudeEffort::Low)])
        );
        let missing = claude(Path::new("claude"), &dir.path().join("none.json"));
        assert_eq!(
            missing.read_model_settings().unwrap(),
            ClaudeModelSettings::default()
        );
    }

    #[test]
    fn set_model_replaces_only_the_model_value() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_file(dir.path());
        let harness = claude(Path::new("claude"), &path);
        harness.set_model(Some("claude-opus-4-1[1m]")).unwrap();
        let mut expected: Value = serde_json::from_str(SETTINGS).unwrap();
        expected["model"] = Value::from("claude-opus-4-1[1m]");
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), expected);
        let rendered = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&rendered).unwrap();
        let keys: Vec<_> = parsed.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(keys, ["$schema", "permissions", "model", "hooks", "env", "modelSettings", "includeCoAuthoredBy"]);

        harness.set_model(Some("default")).unwrap();
        expected.as_object_mut().unwrap().shift_remove("model");
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), expected);
        assert_eq!(harness.read_model_settings().unwrap().model, None);
    }

    #[test]
    fn set_model_on_missing_file_creates_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude").join("settings.json");
        let harness = claude(Path::new("claude"), &path);
        harness.set_model(Some("opus")).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"model\": \"opus\"\n}\n"
        );
        harness.set_model(None).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{}\n");
    }

    #[test]
    fn invalid_model_names_are_rejected_without_writing() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_file(dir.path());
        let harness = claude(Path::new("claude"), &path);
        for bad in ["", "opus 4", "opus\n", "a\u{7}"] {
            let err = harness.set_model(Some(bad)).unwrap_err();
            assert!(matches!(err, Error::Invalid { .. }), "{bad:?}: {err:?}");
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), SETTINGS);
    }

    #[test]
    fn set_effort_appends_and_removes_the_top_level_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_file(dir.path());
        let harness = claude(Path::new("claude"), &path);
        harness.set_effort(Some(ClaudeEffort::XHigh)).unwrap();
        let mut expected: Value = serde_json::from_str(SETTINGS).unwrap();
        expected["effortLevel"] = Value::from("xhigh");
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), expected);
        assert_eq!(
            harness.read_model_settings().unwrap().effort,
            Some(ClaudeEffort::XHigh)
        );
        harness.set_effort(None).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), serde_json::from_str::<Value>(SETTINGS).unwrap());
    }

    #[test]
    fn effort_parse_rejects_session_only_and_unknown_levels() {
        assert_eq!(ClaudeEffort::parse("xhigh").unwrap(), ClaudeEffort::XHigh);
        for bad in ["max", "ultracode", "HIGH", "", "minimal"] {
            assert!(
                matches!(ClaudeEffort::parse(bad), Err(Error::Invalid { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn set_model_effort_keeps_other_fields_and_models() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_file(dir.path());
        let harness = claude(Path::new("claude"), &path);
        harness
            .set_model_effort("opus", Some(ClaudeEffort::High))
            .unwrap();
        let mut expected: Value = serde_json::from_str(SETTINGS).unwrap();
        expected["modelSettings"]["opus"]["effortLevel"] = Value::from("high");
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), expected);
        let read = harness.read_model_settings().unwrap();
        assert_eq!(read.per_model_effort.get("opus"), Some(&ClaudeEffort::High));
        assert_eq!(read.per_model_effort.get("haiku"), Some(&ClaudeEffort::Low));

        // Removing opus's effort leaves its other fields in place.
        harness.set_model_effort("opus", None).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), serde_json::from_str::<Value>(SETTINGS).unwrap());

        // haiku has only an effort level, so its entry goes.
        harness.set_model_effort("haiku", None).unwrap();
        let mut expected: Value = serde_json::from_str(SETTINGS).unwrap();
        expected["modelSettings"].as_object_mut().unwrap().shift_remove("haiku");
        assert_eq!(serde_json::from_str::<Value>(&std::fs::read_to_string(&path).unwrap()).unwrap(), expected);
    }

    #[test]
    fn removing_the_last_model_entry_removes_model_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "{\n  \"model\": \"opus\",\n  \"modelSettings\": {\n    \"opus\": {\"effortLevel\": \"high\"}\n  }\n}\n").unwrap();
        let harness = claude(Path::new("claude"), &path);
        harness.set_model_effort("opus", None).unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"model\": \"opus\"\n}\n"
        );
        harness
            .set_model_effort("sonnet[1m]", Some(ClaudeEffort::Medium))
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{\n  \"model\": \"opus\",\n  \"modelSettings\": {\n    \"sonnet[1m]\": {\n      \"effortLevel\": \"medium\"\n    }\n  }\n}\n"
        );
    }

    #[test]
    fn non_object_settings_are_refused_and_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "[\"model\"]\n").unwrap();
        let harness = claude(Path::new("claude"), &path);
        let err = harness.set_model(Some("opus")).unwrap_err();
        assert!(matches!(err, Error::Unsupported { .. }), "{err:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "[\"model\"]\n");
    }

    #[test]
    fn concurrent_change_is_refused_and_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = settings_file(dir.path());
        let harness = claude(Path::new("claude"), &path);
        let changed = SETTINGS.replace("sonnet", "haiku");
        let err = harness
            .edit(|doc| {
                std::fs::write(&path, &changed).unwrap();
                doc.set(&[MODEL_KEY], Some("opus"))
            })
            .unwrap_err();
        assert!(matches!(err, Error::ChangedOnDisk { .. }), "{err:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), changed);
    }
}
