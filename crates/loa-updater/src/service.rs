//! The updater as one service the app drives from its Updates screen.
//!
//! [`Updater`] owns the persisted state, the feed cache and the installed
//! versions of every watched product. Each step is one call the app runs
//! as a task: [`Updater::detect`] finds what is installed,
//! [`Updater::check`] reads a feed (conditionally), [`Updater::stage`]
//! installs the offered release side by side, [`Updater::gate`] runs the
//! gate checks and records swap or hold, [`Updater::choose`] applies the
//! user's answer to a hold, [`Updater::apply_pending`] swaps cleared
//! versions while no turn runs, and [`Updater::rollback`] goes back one
//! version. [`Updater::overview`] is a cheap snapshot for the screen.

use loa_util::time::now_secs;
use loa_util::LockExt;
use crate::decision::{self, HoldChoice};
use crate::feeds::{self, FeedError, FeedResponse, GitHubFeed, NpmFeed, RateLimit};
use loa_util::fs::write_atomic;
use crate::gates::{
    self, ChangelogGate, CheckResult, FormatGate, GateCheck, GateInputs, GateReport, OmpProbe,
    OmpReadyProbe, OmpSmolSummarizer, RpcGate, ToolGate, UsedTools,
};
use crate::installed;
use crate::layout::{self, Staged};
use crate::product::{CachedFeed, Feed, PrereleasePolicy, Product, ProductKind, Release};
use crate::signature::{self, SignatureError};
use crate::stage::{self, StageError, Stager};
use crate::state::{HeldUpdate, StateError, UpdaterState};
use crate::swap::{Active, Activity, SwapError, SwapOutcome, Swapper};
use ed25519_dalek::VerifyingKey;
use semver::Version;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Errors of updater operations.
#[derive(Debug, thiserror::Error)]
pub enum UpdaterError {
    #[error("unknown product {0}")]
    UnknownProduct(String),
    #[error("{product} is not installed: {reason}")]
    NotInstalled { product: String, reason: String },
    #[error("no update is offered for {0}; check its feed first")]
    NothingOffered(String),
    #[error("{product} {version} is not staged yet")]
    NotStaged { product: String, version: Version },
    #[error("cannot find the executable of {product} {version}: {reason}")]
    NoExecutable { product: String, version: Version, reason: String },
    #[error("feed: {0}")]
    Feed(#[from] FeedError),
    #[error("staging: {0}")]
    Stage(#[from] StageError),
    #[error("swap: {0}")]
    Swap(#[from] SwapError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error("feed cache {path}: {source}")]
    Cache {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("building HTTP client: {0}")]
    Http(#[from] crate::http::HttpError),
}

pub type Result<T, E = UpdaterError> = std::result::Result<T, E>;

/// Where the version in use comes from when the updater hasn't installed one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installed {
    /// The running app, at this version.
    Running(Version),
    /// An executable: a bare name looked up on `PATH`, or a path.
    Executable(PathBuf),
}

/// Gate checks of the omp product: what the app depends on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmpGates {
    /// Tools the app calls, with the argument fields it fills in.
    pub used_tools: UsedTools,
    /// Dotted config keys the app writes.
    pub written_keys: BTreeSet<String>,
    /// Session format migrations the adapter has, as `(from, to)` steps.
    pub migrations: BTreeSet<(u32, u32)>,
    /// RPC protocol versions the adapter speaks.
    pub rpc_supported: RangeInclusive<u32>,
}

impl Default for OmpGates {
    /// What the app uses today: the adapter reads `bash.command` for shell
    /// jobs and the todo list, the tool bridge exposes `read`, `edit`,
    /// `ast_grep`, `ast_edit`, `lsp` and `debug` with omp's own schemas; the
    /// models screen writes model roles, fallback chains, enabled and
    /// disabled providers, enabled models and provider order, and the
    /// adapter's overlay writes `memory.backend`; the adapter reads session
    /// files up to version 3 and migrates 1 and 2; the RPC client speaks
    /// protocols 1 and 2.
    fn default() -> Self {
        let mut used_tools = UsedTools::new();
        used_tools.insert("bash".into(), BTreeSet::from(["command".to_owned()]));
        for tool in ["todo", "read", "edit", "ast_grep", "ast_edit", "lsp", "debug"] {
            used_tools.insert(tool.into(), BTreeSet::new());
        }
        let written_keys = [
            "modelRoles",
            "retry.fallbackChains",
            "enabledModels",
            "enabledProviders",
            "disabledProviders",
            "modelProviderOrder",
            "memory.backend",
        ]
        .map(String::from)
        .into();
        Self { used_tools, written_keys, migrations: BTreeSet::from([(1, 2), (2, 3)]), rpc_supported: 1..=2 }
    }
}

/// Which gate checks a product's updates go through.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Gates {
    /// None; the app itself relies on its signature instead.
    None,
    /// The changelog review only (harnesses driven through their own CLIs).
    Changelog,
    /// Every check of the design: tools, session format, config schema, RPC
    /// protocol, contract tests and changelog.
    Omp(OmpGates),
}

/// One watched product and how the updater treats it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductSpec {
    pub product: Product,
    /// Name shown on the Updates screen.
    pub label: String,
    pub installed: Installed,
    /// Executable inside a staged payload, relative to it, with `{version}`
    /// replaced. `None` derives it: the file itself for single file
    /// artifacts, the `bin` of `package.json` for npm packages.
    pub executable: Option<String>,
    pub gates: Gates,
}

/// Settings of the updater service.
#[derive(Clone, Debug)]
pub struct UpdaterConfig {
    /// Directory holding versions, state and feed cache.
    pub root: PathBuf,
    pub products: Vec<ProductSpec>,
    pub github_api: String,
    pub npm_registry: String,
    /// Raises GitHub's rate limit when set.
    pub github_token: Option<String>,
    /// Keys app updates must be signed with.
    pub trusted_keys: Vec<VerifyingKey>,
    /// Product whose executable reviews changelogs (omp).
    pub reviewer: String,
    /// `--model` of changelog reviews: `@smol` for omp's smol role.
    pub review_model: String,
    /// Limit for one probe of an installed version.
    pub probe_timeout: Duration,
    /// Limit for one changelog review.
    pub review_timeout: Duration,
}

impl UpdaterConfig {
    /// Production feeds, the smol role for reviews, no trusted keys.
    pub fn new(root: impl Into<PathBuf>, products: Vec<ProductSpec>) -> Self {
        Self {
            root: root.into(),
            products,
            github_api: "https://api.github.com".to_owned(),
            npm_registry: "https://registry.npmjs.org".to_owned(),
            github_token: None,
            trusted_keys: Vec::new(),
            reviewer: "omp".to_owned(),
            review_model: "@smol".to_owned(),
            probe_timeout: Duration::from_secs(60),
            review_timeout: Duration::from_secs(180),
        }
    }
}

/// Release asset suffix of this platform, as omp names its binaries
/// (`linux-x64`, `darwin-arm64`).
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// The products the app watches: itself, omp, and the harnesses its
/// adapters drive.
pub fn default_products(app_version: Version) -> Vec<ProductSpec> {
    let platform = platform();
    let github = |owner: &str, repo: &str| Feed::GitHub { owner: owner.into(), repo: repo.into() };
    let npm = |package: &str| Feed::Npm { package: package.into() };
    let product = |id: &str, kind: ProductKind, feed: Feed, asset: Option<String>| Product {
        id: id.into(),
        kind,
        feed,
        prereleases: PrereleasePolicy::Stable,
        asset,
    };
    let harness = |id: &str, label: &str, package: &str| ProductSpec {
        product: product(id, ProductKind::Harness(id.into()), npm(package), None),
        label: label.into(),
        installed: Installed::Executable(id.into()),
        executable: None,
        gates: Gates::Changelog,
    };
    vec![
        ProductSpec {
            product: product(
                "app",
                ProductKind::App,
                github("MrGreenGaming14", "omp-gui"),
                Some(format!("loa-app-{platform}.tar.gz")),
            ),
            label: "loa".into(),
            installed: Installed::Running(app_version),
            executable: Some("loa-app".into()),
            gates: Gates::None,
        },
        ProductSpec {
            product: product("omp", ProductKind::Omp, github("can1357", "oh-my-pi"), Some(format!("omp-{platform}"))),
            label: "omp".into(),
            installed: Installed::Executable("omp".into()),
            executable: None,
            gates: Gates::Omp(OmpGates::default()),
        },
        harness("claude", "Claude Code", "@anthropic-ai/claude-code"),
        harness("codex", "Codex", "@openai/codex"),
        harness("opencode", "OpenCode", "opencode-ai"),
    ]
}

/// Where a product's version in use was found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstalledStatus {
    /// Not looked for yet.
    Unknown,
    /// The running app.
    Running,
    /// A version the updater installed and made active.
    Managed(PathBuf),
    /// An executable the updater didn't install.
    System(PathBuf),
    /// Not found, or it didn't report a version.
    Missing(String),
}

/// Result of checking a staged artifact's signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignatureStatus {
    /// Signed by a trusted key and matching its manifest.
    Verified,
    /// The release has no signature.
    Unsigned,
    /// No trusted key is configured, so nothing can be verified.
    NoTrustedKeys,
    /// Signed, but not by a trusted key, or the artifact changed.
    Rejected(String),
}

/// One product on the Updates screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProductStatus {
    pub id: String,
    pub label: String,
    pub kind: ProductKind,
    /// The feed, e.g. `GitHub can1357/oh-my-pi` or `npm @openai/codex`.
    pub source: String,
    /// Version in use.
    pub current: Option<Version>,
    pub installed: InstalledStatus,
    /// The release to offer over `current`, after pins and waits.
    pub offer: Option<Release>,
    /// Highest version the feed lists, offered or not.
    pub newest: Option<Version>,
    /// Release notes between `current` and the offer, oldest first.
    pub changelog: String,
    pub last_check_unix: Option<u64>,
    /// Fully staged versions on disk, ascending.
    pub staged: Vec<Version>,
    /// The updater's active pointer, when it installed a version.
    pub active: Option<Active>,
    /// Last gate report.
    pub report: Option<GateReport>,
    pub held: Option<HeldUpdate>,
    /// Version cleared to swap in at the next point between sessions.
    pub pending_swap: Option<Version>,
    pub pin: Option<Version>,
    /// Versions up to this one are skipped.
    pub waiting_past: Option<Version>,
    /// Signature of the staged offer (app updates), once checked.
    pub signature: Option<SignatureStatus>,
    /// The app swapped to a version it isn't running yet.
    pub restart_required: bool,
    /// Last error of a step on this product.
    pub error: Option<String>,
}

/// Snapshot of every product for the Updates screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overview {
    pub products: Vec<ProductStatus>,
    pub github_rate_limit: Option<RateLimit>,
}

/// The version and executable a product uses now.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Current {
    version: Version,
    executable: Option<PathBuf>,
}

/// Counts running turns; swaps wait until none runs. The app calls
/// [`TurnGate::begin`] when a turn starts and [`TurnGate::end`] when it ends.
#[derive(Debug)]
pub struct TurnGate(tokio::sync::watch::Sender<usize>);

impl Default for TurnGate {
    fn default() -> Self {
        Self(tokio::sync::watch::Sender::new(0))
    }
}

impl TurnGate {
    pub fn begin(&self) {
        self.0.send_modify(|n| *n += 1);
    }

    pub fn end(&self) {
        self.0.send_modify(|n| *n = n.saturating_sub(1));
    }

    /// Sets the number of running turns, e.g. recounted from the sessions.
    pub fn set(&self, running: usize) {
        self.0.send_replace(running);
    }
}

impl Activity for TurnGate {
    fn turn_running(&self) -> bool {
        *self.0.borrow() > 0
    }
}

/// What looking for a product's executable found: its version and path, or
/// why there is none.
type Detected = Result<(Version, PathBuf), String>;

struct Inner {
    config: UpdaterConfig,
    github: GitHubFeed,
    npm: NpmFeed,
    stager: Stager,
    swapper: Swapper,
    state: Mutex<UpdaterState>,
    cache: Mutex<BTreeMap<String, CachedFeed>>,
    installed: Mutex<BTreeMap<String, Detected>>,
    signatures: Mutex<BTreeMap<(String, Version), SignatureStatus>>,
    errors: Mutex<BTreeMap<String, String>>,
    rate_limit: Mutex<Option<RateLimit>>,
}

/// The updater service. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct Updater {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for Updater {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Updater").field("root", &self.inner.config.root).finish_non_exhaustive()
    }
}


impl Updater {
    /// Opens the updater under `config.root`, loading its state and feed
    /// cache. Nothing is fetched or run.
    pub fn open(config: UpdaterConfig) -> Result<Self> {
        let client = crate::http::client()?;
        Self::with_client(config, client)
    }

    /// Like [`Updater::open`] with a given HTTP client.
    pub fn with_client(config: UpdaterConfig, client: reqwest::Client) -> Result<Self> {
        let state = UpdaterState::load(&config.root)?;
        let cache = load_cache(&config.root)?;
        let mut swapper = Swapper::new(&config.root);
        if let Some(app) = config.products.iter().find(|p| p.product.kind == ProductKind::App) {
            swapper.app_product = app.product.id.clone();
        }
        Ok(Self {
            inner: Arc::new(Inner {
                github: GitHubFeed::new(client.clone(), &config.github_api),
                npm: NpmFeed::new(client.clone(), &config.npm_registry),
                stager: Stager::new(client, &config.root),
                swapper,
                state: Mutex::new(state),
                cache: Mutex::new(cache),
                installed: Mutex::new(BTreeMap::new()),
                signatures: Mutex::new(BTreeMap::new()),
                errors: Mutex::new(BTreeMap::new()),
                rate_limit: Mutex::new(None),
                config,
            }),
        })
    }

    pub fn config(&self) -> &UpdaterConfig {
        &self.inner.config
    }

    fn spec(&self, id: &str) -> Result<&ProductSpec> {
        self.inner
            .config
            .products
            .iter()
            .find(|p| p.product.id == id)
            .ok_or_else(|| UpdaterError::UnknownProduct(id.to_owned()))
    }

    /// Records the outcome of a step: an error is kept for the screen, a
    /// success clears the last one.
    fn note<T>(&self, id: &str, result: Result<T>) -> Result<T> {
        let mut errors = self.inner.errors.locked();
        match &result {
            Ok(_) => {
                errors.remove(id);
            }
            Err(e) => {
                errors.insert(id.to_owned(), e.to_string());
            }
        }
        result
    }

    /// Finds every product's executable and asks it for its version.
    pub async fn detect(&self) {
        let probes = self.inner.config.products.iter().filter_map(|spec| match &spec.installed {
            Installed::Executable(name) => Some(async move {
                let timeout = self.inner.config.probe_timeout;
                let found = match installed::find_executable(name) {
                    Some(path) => installed::executable_version(&path, timeout).await.map(|v| (v, path)),
                    None => Err(format!("{} not found", name.display())),
                };
                (spec.product.id.clone(), found)
            }),
            Installed::Running(_) => None,
        });
        let results = futures::future::join_all(probes).await;
        self.inner.installed.locked().extend(results);
    }

    fn current(&self, spec: &ProductSpec) -> Result<Option<Current>> {
        let id = &spec.product.id;
        if let Installed::Running(version) = &spec.installed {
            return Ok(Some(Current { version: version.clone(), executable: None }));
        }
        if let Some(active) = self.inner.swapper.active(id)? {
            let staged = self.staged_version(id, &active.current)?;
            let executable = staged_executable(spec, &staged).ok();
            return Ok(Some(Current { version: active.current, executable }));
        }
        Ok(self.inner.installed.locked()
            .get(id)
            .and_then(|found| found.as_ref().ok())
            .map(|(version, path)| Current { version: version.clone(), executable: Some(path.clone()) }))
    }

    /// The executable new sessions of `id` should start: the active version
    /// the updater installed, else the one found on the system. Sessions
    /// keep the executable they started with, so a swap only takes effect
    /// for sessions started after it.
    pub fn executable(&self, id: &str) -> Option<PathBuf> {
        let spec = self.spec(id).ok()?;
        self.current(spec).ok().flatten()?.executable
    }

    fn staged_version(&self, id: &str, version: &Version) -> Result<Staged> {
        stage::staged(&self.inner.config.root, id, version)?
            .ok_or_else(|| UpdaterError::NotStaged { product: id.to_owned(), version: version.clone() })
    }

    async fn current_or_detect(&self, spec: &ProductSpec) -> Result<Current> {
        if let Some(current) = self.current(spec)? {
            return Ok(current);
        }
        self.detect().await;
        self.current(spec)?.ok_or_else(|| UpdaterError::NotInstalled {
            product: spec.product.id.clone(),
            reason: match self.inner.installed.locked().get(&spec.product.id) {
                Some(Err(reason)) => reason.clone(),
                _ => "no version found".to_owned(),
            },
        })
    }

    /// The release to offer for `spec` over `current`, from the cache.
    fn offer(&self, spec: &ProductSpec, current: &Version) -> Option<Release> {
        let id = &spec.product.id;
        let cache = self.inner.cache.locked();
        let releases = &cache.get(&feeds::cache_key(&spec.product))?.releases;
        let state = self.inner.state.locked();
        let pin = state.pins.get(id);
        let allowed: Vec<Release> =
            releases.iter().filter(|r| decision::offer_allowed(&state, id, &r.version)).cloned().collect();
        feeds::newest(&allowed, current, spec.product.prereleases, pin).cloned()
    }

    /// Reads `id`'s feed with a conditional request and returns the release
    /// to offer, if any. Products not installed are still checked, against
    /// version 0.0.0, so the screen shows what is available.
    pub async fn check(&self, id: &str) -> Result<Option<Release>> {
        let result = self.check_inner(id).await;
        self.note(id, result)
    }

    async fn check_inner(&self, id: &str) -> Result<Option<Release>> {
        let spec = self.spec(id)?;
        let product = &spec.product;
        let key = feeds::cache_key(product);
        let cached = self.inner.cache.locked().get(&key).cloned();
        let response = match &product.feed {
            Feed::GitHub { .. } => {
                self.inner.github.fetch(product, cached.as_ref(), self.inner.config.github_token.as_deref()).await?
            }
            Feed::Npm { .. } => self.inner.npm.fetch(product, cached.as_ref()).await?,
        };
        match response {
            FeedResponse::NotModified if cached.is_none() => {
                return Err(FeedError::NotModifiedWithoutCache { url: key }.into());
            }
            FeedResponse::NotModified => {}
            FeedResponse::Fresh { feed, rate_limit } => {
                if matches!(product.feed, Feed::GitHub { .. }) && rate_limit.is_some() {
                    *self.inner.rate_limit.locked() = rate_limit;
                }
                self.inner.cache.locked().insert(key, feed);
                self.save_cache()?;
            }
        }
        self.inner.state.locked().last_check_unix.insert(id.to_owned(), now_secs());
        self.save_state()?;
        self.offered(spec).await
    }

    /// The release to offer over the version in use (0.0.0 when nothing is
    /// installed). Looks for the installed executable first when it wasn't
    /// looked for yet.
    async fn offered(&self, spec: &ProductSpec) -> Result<Option<Release>> {
        let detected = self.inner.installed.locked().contains_key(&spec.product.id);
        if !detected && self.current(spec)?.is_none() && matches!(spec.installed, Installed::Executable(_)) {
            self.detect().await;
        }
        let current = self.current(spec)?.map_or_else(|| Version::new(0, 0, 0), |c| c.version);
        Ok(self.offer(spec, &current))
    }

    async fn require_offer(&self, spec: &ProductSpec) -> Result<Release> {
        self.offered(spec).await?.ok_or_else(|| UpdaterError::NothingOffered(spec.product.id.clone()))
    }

    /// Checks every product's feed; returns each product's result.
    pub async fn check_all(&self) -> Vec<(String, Result<Option<Release>>)> {
        let checks = self.inner.config.products.iter().map(|spec| async move {
            let id = spec.product.id.clone();
            let result = self.check(&id).await;
            (id, result)
        });
        futures::future::join_all(checks).await
    }

    /// Downloads, verifies and unpacks the offered release beside the
    /// version in use. App releases have their signature checked right away.
    pub async fn stage(&self, id: &str) -> Result<Version> {
        let result = self.stage_inner(id).await;
        self.note(id, result)
    }

    async fn stage_inner(&self, id: &str) -> Result<Version> {
        let spec = self.spec(id)?;
        let release = self.require_offer(spec).await?;
        let staged = self.inner.stager.stage(&release).await?;
        if spec.product.kind == ProductKind::App {
            self.verify_staged(id, staged).await;
        }
        Ok(release.version)
    }

    /// Checks the signature of the staged offer of `id`.
    pub async fn verify(&self, id: &str) -> Result<SignatureStatus> {
        let spec = self.spec(id)?;
        let release = self.require_offer(spec).await?;
        let staged = self.staged_version(id, &release.version)?;
        Ok(self.verify_staged(id, staged).await)
    }

    async fn verify_staged(&self, id: &str, staged: Staged) -> SignatureStatus {
        let version = staged.manifest.version.clone();
        let keys = self.inner.config.trusted_keys.clone();
        let status = if keys.is_empty() {
            SignatureStatus::NoTrustedKeys
        } else {
            // Hashing a large artifact blocks; keep it off the async workers.
            match tokio::task::spawn_blocking(move || signature::verify_staged(&staged, &keys)).await {
                Ok(Ok(())) => SignatureStatus::Verified,
                Ok(Err(SignatureError::Unsigned { .. })) => SignatureStatus::Unsigned,
                Ok(Err(e)) => SignatureStatus::Rejected(e.to_string()),
                Err(e) => SignatureStatus::Rejected(format!("verification task failed: {e}")),
            }
        };
        self.inner.signatures.locked().insert((id.to_owned(), version), status.clone());
        status
    }

    /// Runs the gate checks of the staged offer against the version in use,
    /// then records swap (pending until between sessions) or hold.
    pub async fn gate(&self, id: &str) -> Result<GateReport> {
        let result = self.gate_inner(id).await;
        self.note(id, result)
    }

    async fn gate_inner(&self, id: &str) -> Result<GateReport> {
        let spec = self.spec(id)?;
        let current = self.current_or_detect(spec).await?;
        let release = self.offer(spec, &current.version).ok_or_else(|| UpdaterError::NothingOffered(id.to_owned()))?;
        let to = release.version.clone();
        let staged = self.staged_version(id, &to)?;
        let report = match &spec.gates {
            Gates::None => GateReport { product: id.to_owned(), from: current.version.clone(), to, checks: Vec::new() },
            gates => {
                let new_executable = staged_executable(spec, &staged)?;
                self.run_checks(spec, gates, &current, &new_executable, &to).await
            }
        };
        let mut state = self.inner.state.locked();
        decision::record(&mut state, &report, now_secs());
        state.reports.insert(id.to_owned(), report.clone());
        state.save(&self.inner.config.root)?;
        Ok(report)
    }

    async fn run_checks(
        &self,
        spec: &ProductSpec,
        gates: &Gates,
        current: &Current,
        new_executable: &Path,
        to: &Version,
    ) -> GateReport {
        let config = &self.inner.config;
        let id = &spec.product.id;
        let notes = self.changelog(spec, &current.version, to);
        // Reviews run on the omp in use, never on an unchecked staged one.
        let reviewer_exe = if *id == config.reviewer {
            current.executable.clone()
        } else {
            self.executable(&config.reviewer)
        };
        let summarizer = reviewer_exe.as_ref().map(|exe| OmpSmolSummarizer {
            executable: exe.clone(),
            timeout: config.review_timeout,
            model: config.review_model.clone(),
        });
        let changelog_gate = match (&summarizer, notes.trim().is_empty()) {
            (Some(summarizer), false) => Some(ChangelogGate { summarizer, text: &notes }),
            _ => None,
        };
        let changelog_note = if notes.trim().is_empty() {
            Some(CheckResult::pass(
                GateCheck::Changelog,
                vec![format!("no release notes published after {}", current.version)],
            ))
        } else if summarizer.is_none() {
            Some(CheckResult::failed_to_run(GateCheck::Changelog, format!("{} is not installed to review it", config.reviewer)))
        } else {
            None
        };

        let mut report = match gates {
            Gates::Omp(omp) => {
                let Some(old) = &current.executable else {
                    return GateReport {
                        product: id.clone(),
                        from: current.version.clone(),
                        to: to.clone(),
                        checks: vec![CheckResult::failed_to_run(GateCheck::ToolSchemas, "the version in use has no executable to compare with")],
                    };
                };
                let probe = OmpProbe::new(omp.used_tools.keys().cloned(), config.probe_timeout);
                let ready = OmpReadyProbe::new(config.probe_timeout);
                let inputs = GateInputs {
                    old_executable: old,
                    new_executable,
                    tools: Some(ToolGate { catalog: &probe, used: &omp.used_tools }),
                    formats: Some(FormatGate {
                        probe: &probe,
                        migrations: &omp.migrations,
                        written_keys: &omp.written_keys,
                    }),
                    rpc: Some(RpcGate { probe: &ready, supported: omp.rpc_supported.clone() }),
                    contract: Some(&probe),
                    changelog: changelog_gate,
                };
                gates::run_gates(id, &current.version, to, &inputs).await
            }
            Gates::Changelog | Gates::None => {
                let old = current.executable.clone().unwrap_or_default();
                let inputs = GateInputs {
                    old_executable: &old,
                    new_executable,
                    tools: None,
                    formats: None,
                    rpc: None,
                    contract: None,
                    changelog: changelog_gate,
                };
                gates::run_gates(id, &current.version, to, &inputs).await
            }
        };
        report.checks.extend(changelog_note);
        report
    }

    /// Release notes between `from` (exclusive) and `to`, oldest first.
    fn changelog(&self, spec: &ProductSpec, from: &Version, to: &Version) -> String {
        let cache = self.inner.cache.locked();
        let Some(feed) = cache.get(&feeds::cache_key(&spec.product)) else {
            return String::new();
        };
        let notes: Vec<(Version, &str)> = feeds::between(&feed.releases, from, to)
            .into_iter()
            .filter(|r| !r.notes.trim().is_empty())
            .map(|r| (r.version.clone(), r.notes.as_str()))
            .collect();
        if notes.is_empty() { String::new() } else { gates::changelog_text(&notes) }
    }

    /// Applies the user's answer to `id`'s held update. Returns the version
    /// now pending swap, for update anyway.
    pub fn choose(&self, id: &str, choice: HoldChoice) -> Result<Option<Version>> {
        let result = (|| {
            let spec = self.spec(id)?;
            let current = self
                .current(spec)?
                .ok_or_else(|| UpdaterError::NotInstalled { product: id.to_owned(), reason: "no version in use".into() })?;
            let mut state = self.inner.state.locked();
            let to = decision::apply_choice(&mut state, id, &current.version, choice);
            state.save(&self.inner.config.root)?;
            Ok(to)
        })();
        self.note(id, result)
    }

    /// Removes `id`'s pin and wait, so every newer version is offered again.
    pub fn unpin(&self, id: &str) -> Result<()> {
        self.spec(id)?;
        let mut state = self.inner.state.locked();
        state.pins.remove(id);
        state.waits.remove(id);
        state.save(&self.inner.config.root)?;
        Ok(())
    }

    /// Replaces every pin with `pins` (product id to version), e.g. the
    /// pins of the user's config file. Unknown products are refused.
    pub fn set_pins(&self, pins: BTreeMap<String, Version>) -> Result<()> {
        for id in pins.keys() {
            self.spec(id)?;
        }
        let mut state = self.inner.state.locked();
        if state.pins == pins {
            return Ok(());
        }
        state.pins = pins;
        state.save(&self.inner.config.root)?;
        Ok(())
    }

    /// Swaps in every version cleared for swapping, unless a turn runs
    /// (those stay pending). Returns each attempted product's outcome.
    pub fn apply_pending(&self, activity: &dyn Activity) -> Vec<(String, Result<SwapOutcome>)> {
        let pending: Vec<String> = self.inner.state.locked().pending_swap.keys().cloned().collect();
        pending
            .into_iter()
            .map(|id| {
                let outcome = self.swap(&id, activity);
                (id, outcome)
            })
            .collect()
    }

    /// Swaps in `id`'s pending version unless a turn runs. App versions must
    /// carry a trusted signature.
    pub fn swap(&self, id: &str, activity: &dyn Activity) -> Result<SwapOutcome> {
        let result = (|| {
            self.spec(id)?;
            let Some(to) = self.inner.state.locked().pending_swap.get(id).cloned() else {
                return Err(UpdaterError::NothingOffered(id.to_owned()));
            };
            let keys = self.inner.config.trusted_keys.as_slice();
            let outcome = self.inner.swapper.try_swap(id, &to, Some(keys).filter(|k| !k.is_empty()), activity);
            let mut state = self.inner.state.locked();
            match &outcome {
                Ok(SwapOutcome::Deferred) => {}
                // A version that can't be swapped in stays out until checked
                // again, instead of failing after every turn.
                Ok(_) | Err(_) => {
                    state.pending_swap.remove(id);
                }
            }
            state.save(&self.inner.config.root)?;
            Ok(outcome?)
        })();
        self.note(id, result)
    }

    /// Makes the previous version of `id` current again, unless a turn runs.
    pub fn rollback(&self, id: &str, activity: &dyn Activity) -> Result<SwapOutcome> {
        let result = (|| {
            self.spec(id)?;
            let keys = self.inner.config.trusted_keys.as_slice();
            let outcome = self.inner.swapper.rollback(id, Some(keys).filter(|k| !k.is_empty()), activity)?;
            if matches!(outcome, SwapOutcome::Swapped { .. }) {
                let mut state = self.inner.state.locked();
                state.pending_swap.remove(id);
                state.save(&self.inner.config.root)?;
            }
            Ok(outcome)
        })();
        self.note(id, result)
    }

    /// A snapshot of every product. Reads the active pointers and staged
    /// directories; runs nothing.
    pub fn overview(&self) -> Overview {
        let products = self.inner.config.products.iter().map(|spec| self.status(spec)).collect();
        Overview { products, github_rate_limit: *self.inner.rate_limit.locked() }
    }

    fn status(&self, spec: &ProductSpec) -> ProductStatus {
        let id = spec.product.id.clone();
        let mut error = self.inner.errors.locked().get(&id).cloned();
        let active = self.inner.swapper.active(&id).unwrap_or_else(|e| {
            error.get_or_insert_with(|| e.to_string());
            None
        });
        let current = self.current(spec).ok().flatten();
        let installed = match (&spec.installed, &active) {
            (Installed::Running(_), _) => InstalledStatus::Running,
            (_, Some(_)) => match current.as_ref().and_then(|c| c.executable.clone()) {
                Some(path) => InstalledStatus::Managed(path),
                None => InstalledStatus::Missing("the active version has no executable".into()),
            },
            (Installed::Executable(_), None) => match self.inner.installed.locked().get(&id) {
                None => InstalledStatus::Unknown,
                Some(Ok((_, path))) => InstalledStatus::System(path.clone()),
                Some(Err(reason)) => InstalledStatus::Missing(reason.clone()),
            },
        };
        let version = current.as_ref().map_or_else(|| Version::new(0, 0, 0), |c| c.version.clone());
        let offer = self.offer(spec, &version);
        let newest = self.inner.cache.locked()
            .get(&feeds::cache_key(&spec.product))
            .and_then(|feed| {
                feed.releases
                    .iter()
                    .filter(|r| spec.product.prereleases == PrereleasePolicy::Include || r.version.pre.is_empty())
                    .map(|r| r.version.clone())
                    .max()
            });
        let changelog = offer.as_ref().map(|o| self.changelog(spec, &version, &o.version)).unwrap_or_default();
        let signature = offer
            .as_ref()
            .and_then(|o| self.inner.signatures.locked().get(&(id.clone(), o.version.clone())).cloned());
        let restart_required = matches!(&spec.installed, Installed::Running(running)
            if active.as_ref().is_some_and(|a| a.current != *running));
        let state = self.inner.state.locked();
        ProductStatus {
            label: spec.label.clone(),
            kind: spec.product.kind.clone(),
            source: match &spec.product.feed {
                Feed::GitHub { owner, repo } => format!("GitHub {owner}/{repo}"),
                Feed::Npm { package } => format!("npm {package}"),
            },
            current: current.map(|c| c.version),
            installed,
            offer,
            newest,
            changelog,
            last_check_unix: state.last_check_unix.get(&id).copied(),
            staged: staged_versions(&self.inner.config.root, &id),
            active,
            report: state.reports.get(&id).cloned(),
            held: state.held.get(&id).cloned(),
            pending_swap: state.pending_swap.get(&id).cloned(),
            pin: state.pins.get(&id).cloned(),
            waiting_past: state.waits.get(&id).cloned(),
            signature,
            restart_required,
            error,
            id,
        }
    }

    fn save_state(&self) -> Result<()> {
        Ok(self.inner.state.locked().save(&self.inner.config.root)?)
    }

    fn save_cache(&self) -> Result<()> {
        let path = layout::cache_path(&self.inner.config.root);
        let bytes = serde_json::to_vec(&*self.inner.cache.locked())
            .map_err(|e| UpdaterError::Cache { path: path.clone(), source: io::Error::other(e) })?;
        write_atomic(&path, &bytes, None).map_err(|source| UpdaterError::Cache { path, source })
    }
}

fn load_cache(root: &Path) -> Result<BTreeMap<String, CachedFeed>> {
    let path = layout::cache_path(root);
    match std::fs::read(&path) {
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(cache) => Ok(cache),
            // The cache only saves requests; a damaged one is dropped.
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %e, "discarding unreadable feed cache");
                Ok(BTreeMap::new())
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
        Err(source) => Err(UpdaterError::Cache { path, source }),
    }
}

/// Fully staged versions of `product` under `root`, ascending.
fn staged_versions(root: &Path, product: &str) -> Vec<Version> {
    let Ok(entries) = std::fs::read_dir(layout::product_dir(root, product)) else {
        return Vec::new();
    };
    let mut versions: Vec<Version> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let version = Version::parse(entry.file_name().to_str()?).ok()?;
            entry.path().join(layout::MANIFEST_FILE).is_file().then_some(version)
        })
        .collect();
    versions.sort();
    versions
}

/// The executable inside a staged version's payload.
fn staged_executable(spec: &ProductSpec, staged: &Staged) -> Result<PathBuf> {
    let version = &staged.manifest.version;
    let payload = staged.payload();
    let no_executable = |reason: String| UpdaterError::NoExecutable {
        product: spec.product.id.clone(),
        version: version.clone(),
        reason,
    };
    let path = match &spec.executable {
        Some(pattern) => payload.join(pattern.replace("{version}", &version.to_string())),
        None => {
            let single = payload.join(&staged.manifest.artifact_name);
            let package = payload.join("package.json");
            if single.is_file() {
                single
            } else if package.is_file() {
                payload.join(npm_bin(&package, &spec.product.id).map_err(no_executable)?)
            } else {
                return Err(no_executable("the payload is neither a single file nor an npm package".into()));
            }
        }
    };
    if !path.starts_with(&payload) || !path.is_file() {
        return Err(no_executable(format!("{} is not a file in the payload", path.display())));
    }
    Ok(path)
}

/// The `bin` entry of an npm `package.json`: the string form, the entry
/// named like the product, or the only entry.
fn npm_bin(package: &Path, product: &str) -> Result<String, String> {
    let text = std::fs::read_to_string(package).map_err(|e| format!("reading {}: {e}", package.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", package.display()))?;
    let bin = match json.get("bin") {
        Some(serde_json::Value::String(path)) => Some(path.clone()),
        Some(serde_json::Value::Object(map)) => map
            .get(product)
            .or_else(|| (map.len() == 1).then(|| map.values().next()).flatten())
            .and_then(|v| v.as_str())
            .map(str::to_owned),
        _ => None,
    };
    let bin = bin.ok_or_else(|| "package.json names no executable".to_owned())?;
    if bin.split(['/', '\\']).any(|part| part == "..") {
        return Err(format!("package.json bin {bin:?} leaves the package"));
    }
    Ok(bin)
}
