//! Live checks against the real npm and GitHub feeds and the installed omp.
//! The installed omp is only read and run; it is never replaced.
//!
//! Run with `cargo test -p loa-updater --test live -- --ignored`.

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::Response;
use axum::routing::get;
use loa_updater::decision::HoldChoice;
use loa_updater::feeds::{FeedResponse, GitHubFeed, NpmFeed};
use loa_updater::gates::{
    ChangelogGate, FormatGate, GateCheck, GateInputs, OmpProbe, OmpReadyProbe, OmpSmolSummarizer,
    RpcGate, ToolGate, Verdict, changelog_text, run_gates,
};
use loa_updater::installed::{executable_version, find_executable};
use loa_updater::swap::SwapOutcome;
use loa_updater::{
    Feed, Gates, Installed, InstalledStatus, OmpGates, PrereleasePolicy, Product, ProductKind,
    ProductSpec, TurnGate, Updater, UpdaterConfig,
};
use semver::Version;
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

/// The model reviewing changelogs in live runs.
fn live_model() -> String {
    std::env::var("LOA_LIVE_MODEL").unwrap_or_else(|_| "anthropic/claude-haiku-5-5".to_owned())
}

fn installed_omp() -> PathBuf {
    find_executable(std::path::Path::new("omp")).expect("omp on PATH")
}

async fn installed_version() -> Version {
    executable_version(&installed_omp(), Duration::from_secs(30)).await.expect("omp --version")
}

fn client() -> reqwest::Client {
    loa_updater::http::client().expect("client")
}

fn omp_npm() -> Product {
    Product {
        id: "omp".into(),
        kind: ProductKind::Omp,
        feed: Feed::Npm { package: "@oh-my-pi/pi-coding-agent".into() },
        prereleases: PrereleasePolicy::Stable,
        asset: None,
    }
}

fn omp_github() -> Product {
    Product {
        id: "omp".into(),
        kind: ProductKind::Omp,
        feed: Feed::GitHub { owner: "can1357".into(), repo: "oh-my-pi".into() },
        prereleases: PrereleasePolicy::Stable,
        asset: Some(format!("omp-{}", loa_updater::service::platform())),
    }
}

#[tokio::test]
#[ignore = "live: reads the real npm registry"]
async fn live_npm_feed_lists_installed_omp_and_revalidates() {
    let installed = installed_version().await;
    let feed = NpmFeed::new(client(), "https://registry.npmjs.org");
    let FeedResponse::Fresh { feed: cached, .. } = feed.fetch(&omp_npm(), None).await.expect("npm feed") else {
        panic!("first request must be fresh");
    };
    let listed = cached.releases.iter().find(|r| r.version == installed).expect("installed version listed on npm");
    let artifact = listed.artifact.as_ref().expect("tarball");
    assert!(artifact.url.ends_with(".tgz"), "{}", artifact.url);
    assert!(artifact.checksum.is_some(), "integrity becomes a checksum");
    let etag = cached.etag.clone().expect("npm sends an ETag");
    println!("npm: {} versions, installed {installed}, etag {etag}", cached.releases.len());

    let again = feed.fetch(&omp_npm(), Some(&cached)).await.expect("conditional npm request");
    assert_eq!(again, FeedResponse::NotModified, "If-None-Match revalidates");
}

#[tokio::test]
#[ignore = "live: reads the real GitHub releases API"]
async fn live_github_feed_has_installed_omp_binary_with_digest() {
    let installed = installed_version().await;
    let feed = GitHubFeed::new(client(), "https://api.github.com");
    let token = std::env::var("GITHUB_TOKEN").ok();
    let FeedResponse::Fresh { feed: cached, rate_limit } =
        feed.fetch(&omp_github(), None, token.as_deref()).await.expect("GitHub feed")
    else {
        panic!("first request must be fresh");
    };
    let listed = cached.releases.iter().find(|r| r.version == installed).expect("installed version released");
    let artifact = listed.artifact.as_ref().expect("platform binary");
    assert!(artifact.checksum.is_some(), "GitHub digest becomes a checksum");
    assert!(!listed.notes.trim().is_empty(), "release notes");
    println!("github: {} releases, rate limit {rate_limit:?}", cached.releases.len());

    let again = feed.fetch(&omp_github(), Some(&cached), token.as_deref()).await.expect("conditional request");
    assert_eq!(again, FeedResponse::NotModified);
}

/// Release notes of the installed version from GitHub.
async fn installed_notes(installed: &Version) -> String {
    let feed = GitHubFeed::new(client(), "https://api.github.com");
    let token = std::env::var("GITHUB_TOKEN").ok();
    let FeedResponse::Fresh { feed, .. } = feed.fetch(&omp_github(), None, token.as_deref()).await.expect("feed") else {
        unreachable!("no cache was sent");
    };
    feed.releases.into_iter().find(|r| r.version == *installed).map(|r| r.notes).unwrap_or_default()
}

#[tokio::test]
#[ignore = "live: runs the installed omp and one smol model call"]
async fn live_gates_pass_with_installed_omp_as_old_and_new() {
    let omp = installed_omp();
    let installed = installed_version().await;
    let gates = OmpGates::default();
    let probe = OmpProbe::new(gates.used_tools.keys().cloned(), Duration::from_secs(60));
    let ready = OmpReadyProbe::new(Duration::from_secs(60));
    let summarizer = OmpSmolSummarizer { executable: omp.clone(), timeout: Duration::from_secs(180), model: live_model() };
    let notes = installed_notes(&installed).await;
    let text = changelog_text(&[(installed.clone(), notes.as_str())]);
    let inputs = GateInputs {
        old_executable: &omp,
        new_executable: &omp,
        tools: Some(ToolGate { catalog: &probe, used: &gates.used_tools }),
        formats: Some(FormatGate { probe: &probe, migrations: &gates.migrations, written_keys: &gates.written_keys }),
        rpc: Some(RpcGate { probe: &ready, supported: gates.rpc_supported.clone() }),
        contract: Some(&probe),
        changelog: Some(ChangelogGate { summarizer: &summarizer, text: &text }),
    };
    let report = run_gates("omp", &installed, &installed, &inputs).await;
    println!("{report}");
    let checks: Vec<GateCheck> = report.checks.iter().map(|c| c.check).collect();
    assert_eq!(
        checks,
        [
            GateCheck::ToolSchemas,
            GateCheck::SessionFormat,
            GateCheck::ConfigSchema,
            GateCheck::RpcProtocol,
            GateCheck::ContractTests,
            GateCheck::Changelog
        ]
    );
    for check in &report.checks {
        if check.check == GateCheck::Changelog {
            // The reviewer ran; whether it flags entries depends on the notes.
            assert!(
                !check.details.iter().any(|d| d.starts_with("check could not run")),
                "changelog review ran: {:?}",
                check.details
            );
        } else {
            assert_eq!(check.verdict, Verdict::Pass, "{}: {:?}", check.check.label(), check.details);
        }
    }
    let session = report.checks.iter().find(|c| c.check == GateCheck::SessionFormat).unwrap();
    assert!(session.details[0].starts_with("unchanged at "), "{:?}", session.details);
}

#[derive(Clone)]
struct Fixture {
    releases: Arc<String>,
    artifact: Arc<Vec<u8>>,
}

async fn releases(State(f): State<Fixture>) -> Response<Body> {
    Response::builder().header("etag", "\"live\"").body(Body::from((*f.releases).clone())).unwrap()
}

async fn artifact(State(f): State<Fixture>) -> Response<Body> {
    Response::builder().body(Body::from((*f.artifact).clone())).unwrap()
}

/// The whole flow through the service: a local feed offers the next patch
/// version of omp, whose artifact is a wrapper running the installed omp.
/// Staging, all gate checks (with a real changelog review) and the swap run
/// for real inside a temporary root; the installed omp stays untouched.
#[tokio::test]
#[ignore = "live: runs the installed omp and one smol model call"]
async fn live_service_checks_stages_gates_and_swaps_a_wrapped_installed_omp() {
    let omp = installed_omp();
    let installed = installed_version().await;
    let next = Version::new(installed.major, installed.minor, installed.patch + 1);
    let notes = installed_notes(&installed).await;
    let wrapper = format!("#!/bin/sh\nexec '{}' \"$@\"\n", omp.display()).into_bytes();
    let digest = hex::encode(Sha256::digest(&wrapper));
    let asset = format!("omp-{}", loa_updater::service::platform());
    let fixture = Fixture { releases: Arc::new(String::new()), artifact: Arc::new(wrapper.clone()) };
    // The release list needs the server's address, so bind first.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let body = serde_json::json!([{
        "tag_name": format!("v{next}"),
        "body": notes,
        "assets": [{"name": asset, "browser_download_url": format!("{base}/artifact"), "size": wrapper.len(), "digest": format!("sha256:{digest}")}]
    }]);
    let fixture = Fixture { releases: Arc::new(body.to_string()), ..fixture };
    let router = Router::new()
        .route("/repos/can1357/oh-my-pi/releases", get(releases))
        .route("/artifact", get(artifact))
        .with_state(fixture);
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });

    let root = tempfile::tempdir().unwrap();
    let spec = ProductSpec {
        product: omp_github(),
        label: "omp".into(),
        installed: Installed::Executable(omp.clone()),
        executable: None,
        gates: Gates::Omp(OmpGates::default()),
    };
    let mut config = UpdaterConfig::new(root.path(), vec![spec]);
    config.github_api = base.clone();
    config.review_model = live_model();
    let updater = Updater::open(config).unwrap();

    updater.detect().await;
    let offer = updater.check("omp").await.unwrap().expect("next version offered");
    assert_eq!(offer.version, next);
    assert_eq!(updater.stage("omp").await.unwrap(), next);
    let report = updater.gate("omp").await.unwrap();
    println!("{report}");
    assert_eq!(report.checks.len(), 6, "every omp check ran");

    let status = updater.overview().products.remove(0);
    assert_eq!(status.current, Some(installed.clone()));
    assert_eq!(status.installed, InstalledStatus::System(omp.clone()));
    assert_eq!(status.staged, vec![next.clone()]);
    if report.is_clean() {
        assert_eq!(status.pending_swap, Some(next.clone()));
    } else {
        // The reviewer flagged entries: hold, then update anyway.
        assert!(status.held.is_some());
        assert_eq!(updater.choose("omp", HoldChoice::UpdateAnyway).unwrap(), Some(next.clone()));
    }

    let turns = TurnGate::default();
    turns.begin();
    assert_eq!(updater.swap("omp", &turns).unwrap(), SwapOutcome::Deferred, "never mid turn");
    turns.end();
    assert_eq!(
        updater.swap("omp", &turns).unwrap(),
        SwapOutcome::Swapped { from: None, to: next.clone() }
    );
    let managed = updater.executable("omp").expect("managed executable");
    assert!(managed.starts_with(root.path()), "{}", managed.display());
    assert_eq!(executable_version(&managed, Duration::from_secs(30)).await.unwrap(), installed, "wrapper runs omp");
    let status = updater.overview().products.remove(0);
    assert_eq!(status.current, Some(next));
    assert!(matches!(status.installed, InstalledStatus::Managed(_)));
    assert_eq!(installed_version().await, installed, "the installed omp is untouched");
}
