//! Opt-in tests against the installed `omp`. They list providers and models
//! and switch a model; none of that makes a model call. No login is started
//! and the user's own settings files are never written.
//! Run with `cargo test -p loa-models --test live -- --ignored`.
//! `LOA_LIVE_MODEL` (`provider/id`) picks the model to switch to.

use std::sync::Arc;

use loa_models::config::{OmpConfig, Selector};
use loa_models::models_yml::{ApiKeyWrite, CustomModel, CustomProvider, KeyReference, ModelsYml};
use loa_models::omp::{OmpClient, OmpFiles, OmpLaunch};
use loa_models::process::TokioProcessRunner;
use loa_models::rpc::OmpRpc;
use loa_models::validate::{OmpSettingsValidator, SettingsValidator};

fn live_model() -> (String, String) {
    let selector = std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".into());
    let (provider, id) = selector.split_once('/').expect("LOA_LIVE_MODEL is provider/id");
    (provider.to_owned(), id.to_owned())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp (no model call)"]
async fn real_omp_lists_providers_and_models_and_switches_model() {
    let cwd = tempfile::tempdir().unwrap();
    let launch = OmpLaunch { cwd: Some(cwd.path().to_owned()), ..OmpLaunch::default() };
    let omp = OmpClient::spawn(&launch).await.expect("omp starts in rpc mode");

    let providers = omp.get_login_providers().await.unwrap();
    assert!(providers.iter().any(|p| p.id == "anthropic"), "{providers:?}");
    assert!(providers.iter().all(|p| !p.name.is_empty()));

    let models = omp.get_available_models().await.unwrap();
    let (provider, id) = live_model();
    assert!(
        models.iter().any(|m| m.provider == provider && m.id == id),
        "{provider}/{id} not among {:?}",
        models.iter().map(|m| m.selector()).collect::<Vec<_>>()
    );

    let selected = omp.set_model(&provider, &id).await.unwrap();
    assert_eq!(selected.selector(), format!("{provider}/{id}"));
    let current = omp.current_model().await.unwrap().expect("a current model");
    assert_eq!(current.selector(), format!("{provider}/{id}"));

    let err = omp.set_model("no-such-provider", "x").await.unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    omp.shutdown(std::time::Duration::from_secs(5)).await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "runs the installed omp (no model call)"]
async fn real_omp_reports_its_agent_dir_and_validates_settings_in_a_temp_dir() {
    let agent = tempfile::tempdir().unwrap();
    let launch = OmpLaunch::default().env("PI_CODING_AGENT_DIR", agent.path());
    let files = OmpFiles::resolve(&launch).await.unwrap();
    assert_eq!(std::fs::canonicalize(&files.agent_dir).unwrap(), std::fs::canonicalize(agent.path()).unwrap());

    let validator = OmpSettingsValidator::new("omp", Arc::new(TokioProcessRunner));
    let mut config = OmpConfig::load(&files.config()).unwrap();
    config.set_model_role("smol", &[Selector::parse("anthropic/claude-haiku-5-5:low").unwrap()]).unwrap();
    config.save_validated(&validator).await.expect("omp accepts the role");
    assert!(std::fs::read_to_string(files.config()).unwrap().contains("smol: anthropic/claude-haiku-5-5:low"));

    let mut models = ModelsYml::load(&files.models()).unwrap();
    models
        .set_custom_provider(&CustomProvider {
            id: "local-gw".into(),
            base_url: Some("http://127.0.0.1:9/v1".into()),
            api: Some("openai-completions".into()),
            api_key: ApiKeyWrite::Set(KeyReference::env_var("LOCAL_GW_KEY").unwrap()),
            models: vec![CustomModel { id: "m1".into(), ..Default::default() }],
            ..Default::default()
        })
        .unwrap();
    models.save_validated(&validator).await.expect("omp accepts the provider");
    assert!(validator.check_models("providers:\n  gw:\n    api: openai-completions\n    models:\n      - id: m1\n").await.is_err());
}
