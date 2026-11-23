//! The service against a fake omp RPC, an offline omp account and a temp agent dir: the
//! catalog, settings writes and signing out of omp. Logins and sign outs through the real
//! harness accounts are tested in `loa-harness/tests/models_service.rs`.

use loa_settings::models::files::OmpFiles;
use loa_settings::models::keys::EnvironmentPresence;
use loa_settings::models::login::{Auth, ProviderKey, UrlOpener};
use loa_settings::models::rpc::{self, LoginProvider, LogoutAccount};
use loa_settings::models::selector::ModelRef;
use crate::service::{Backend, FilesSource, OMP, Parts};
use loa_settings::models::settings::{RoleChain, SettingsValidator};
use crate::testing::{FakeOmp, OfflineOmpAccount};
use async_trait::async_trait;
use std::path::Path;
use std::sync::Arc;

/// Accepts every file unless it contains `reject-me`.
struct Validator;

#[async_trait]
impl SettingsValidator for Validator {
    async fn check_config(&self, text: &str, _: &[(String, serde_json::Value)]) -> loa_settings::models::settings::Result<()> {
        match text.contains("reject-me") {
            true => Err(loa_settings::models::settings::Error::Rejected { what: "config.yml".into(), message: "bad role".into() }),
            false => Ok(()),
        }
    }
    async fn check_models(&self, _: &str) -> loa_settings::models::settings::Result<()> { Ok(()) }
}

struct NoBrowser;

impl UrlOpener for NoBrowser {
    fn open_url(&self, _: &str) -> Result<(), String> { Err("no browser in tests".into()) }
}

struct NoEnv;

impl EnvironmentPresence for NoEnv {
    fn contains(&self, _: &str) -> bool { false }
}

struct Fixture {
    dir: tempfile::TempDir,
    backend: Backend,
}

fn fixture(config: &str) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("config.yml"), config).unwrap();
    std::fs::write(dir.path().join("models.yml"), "").unwrap();
    let backend = Backend::new(Parts {
        rpc: Arc::new(omp_rpc()),
        omp_account: Arc::new(OfflineOmpAccount),
        files: FilesSource::Fixed(OmpFiles::new(dir.path())),
        validator: Arc::new(Validator),
        opener: Arc::new(NoBrowser),
        harnesses: Vec::new(),
        environment: Arc::new(NoEnv),
    });
    Fixture { dir, backend }
}

fn omp_rpc() -> FakeOmp {
    let provider = |id: &str, name: &str, available, authenticated| LoginProvider { id: id.into(), name: name.into(), available, authenticated };
    let model = |provider: &str, id: &str| rpc::ModelInfo {
        id: id.into(), name: id.to_uppercase(), provider: provider.into(), api: String::new(), reasoning: false, input: Vec::new(),
        cost: rpc::ModelCost::default(), context_window: None, max_tokens: None, thinking: None, extra: Default::default(),
    };
    let mut rpc = FakeOmp::default();
    rpc.providers = vec![provider("anthropic", "Anthropic (Claude Pro/Max)", true, true), provider("openai-codex", "ChatGPT Plus/Pro (Codex Subscription)", true, false)];
    rpc.models = vec![model("anthropic", "claude-x"), model("openai-codex", "gpt-y")];
    let credential = |id, provider: &str, label: &str, active| LogoutAccount {
        credential_id: id, provider: provider.into(), label: label.into(), detail: "OAuth".into(), kind: "oauth".into(), active,
    };
    rpc.credentials = vec![credential(7, "anthropic", "me@example.com", true), credential(8, "openai-codex", "me@example.com", true), credential(9, "anthropic", "work@example.com", false)].into();
    rpc
}

#[tokio::test]
async fn the_catalog_resolves_roles_and_assigns_them_in_omp_config() {
    let f = fixture("modelRoles:\n  smol: anthropic/claude-x:low # fast\n  alias: '@smol'\n  missing: absent/model\n");
    let catalog = f.backend.catalog().await.unwrap();
    assert_eq!((catalog.models.len(), catalog.models[0].provider_name.as_str()), (2, "Anthropic (Claude Pro/Max)"));
    let roles: Vec<(&str, &ModelRef)> = catalog.roles.iter().map(|r| (r.role.as_str(), &r.model)).collect();
    let claude = ModelRef::new("anthropic", "claude-x");
    assert_eq!(roles, [("smol", &claude), ("alias", &claude)], "aliases resolve; unavailable models drop out");
    f.backend.assign_role("smol".into(), ModelRef::new("openai-codex", "gpt-y")).await.unwrap();
    let text = std::fs::read_to_string(f.dir.path().join("config.yml")).unwrap();
    assert!(text.starts_with("modelRoles:\n  smol: openai-codex/gpt-y:low # fast\n"), "{text}");
}

#[tokio::test]
async fn settings_save_through_validation_and_refuse_stale_files() {
    let config = Path::new("config.yml");
    let f = fixture("# keep me\nmodelRoles:\n  default: anthropic/claude-x\n");
    let read = || std::fs::read_to_string(f.dir.path().join(config)).unwrap();
    let load = f.backend.load_settings().await.unwrap();
    assert_eq!(load.known_models, ["anthropic/claude-x", "openai-codex/gpt-y"]);
    let mut settings = load.settings.clone();
    settings.roles[0].chain.push("openai-codex/gpt-y".into());
    let saved = f.backend.save_settings(settings).await.unwrap();
    assert_eq!(saved.roles, [RoleChain { role: "default".into(), chain: vec!["anthropic/claude-x".into(), "openai-codex/gpt-y".into()] }]);
    let text = read();
    assert!(text.starts_with("# keep me\n") && text.contains("fallbackChains:\n    default:\n      - openai-codex/gpt-y\n"), "{text}");

    // omp's rejection comes back and nothing is written.
    let mut rejected = saved.clone();
    rejected.roles.push(RoleChain { role: "reject-me".into(), chain: vec!["anthropic/claude-x".into()] });
    assert_eq!(f.backend.save_settings(rejected).await.unwrap_err()[0].location, "config.yml");
    assert_eq!(read(), text);

    // A change made outside the app since the load is not overwritten.
    std::fs::write(f.dir.path().join(config), "modelRoles: {}\n").unwrap();
    let mut stale = saved;
    stale.roles[0].chain.pop();
    let errors = f.backend.save_settings(stale).await.unwrap_err();
    assert!(errors[0].message.contains("changed on disk"), "{errors:?}");
}

#[tokio::test]
async fn signing_out_of_omp_removes_one_stored_credential_at_a_time() {
    let f = fixture("");
    let anthropic = ProviderKey::new(OMP, "anthropic");
    let signed_in = || async { f.backend.providers().await.into_iter().find(|row| row.key == anthropic).unwrap().auth };
    assert_eq!(signed_in().await, Auth::Login { signed_in: true, account: None });
    let credentials = f.backend.credentials(&anthropic).await.unwrap();
    let ids: Vec<(Option<i64>, &str, bool)> = credentials.iter().map(|c| (c.id, c.label.as_str(), c.active)).collect();
    assert_eq!(ids, [(Some(7), "me@example.com", true), (Some(9), "work@example.com", false)], "only the provider's own, as omp lists them");

    assert_eq!(f.backend.sign_out(&anthropic, Some(7)).await, Ok(None));
    assert_eq!(signed_in().await, Auth::Login { signed_in: true, account: None }, "another credential still signs in");
    let gone = f.backend.sign_out(&anthropic, Some(7)).await.unwrap_err();
    assert!(gone.contains("Credential 7 is not stored for anthropic"), "{gone}");
    assert_eq!(f.backend.sign_out(&anthropic, None).await, Err("anthropic has no such stored sign in".into()), "omp removes credentials by id only");

    assert_eq!(f.backend.sign_out(&anthropic, Some(9)).await, Ok(None));
    assert_eq!(signed_in().await, Auth::Login { signed_in: false, account: None });
    assert_eq!(f.backend.credentials(&anthropic).await, Ok(Vec::new()));
    assert_eq!(f.backend.credentials(&ProviderKey::new("codex", "codex")).await, Err("unknown harness codex".into()));
}
