//! The updater as one service the Updates screen drives: [`Updater::detect`] finds what is
//! installed, [`Updater::check`] reads a feed, [`Updater::stage`] installs the offer side by
//! side, [`Updater::gate`] records swap or hold, [`Updater::choose`] answers a hold,
//! [`Updater::apply_pending`] swaps between turns and [`Updater::rollback`] goes back one.

use crate::decision::{self, HoldChoice};
use crate::feeds::{self, FeedResponse, FeedSource};
use crate::gates::{self, CheckResult, GateCheck, GateInputs, GatePorts, GateReport, OmpGates};
use crate::model::*;
use crate::stage::{self, MANIFEST_FILE, Staged, Stager};
use crate::state::UpdaterState;
use crate::swap::{Activity, Swapper};
use crate::{Error, Result};
use ed25519_dalek::VerifyingKey;
use loa_util::LockExt;
use loa_util::time::now_secs;
use semver::Version;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
    /// The gate adapters (omp-backed in the app); without them only signature-free checks run.
    pub ports: Option<Arc<dyn GatePorts>>,
    /// Product whose executable reviews changelogs, and the `--model` it reviews with.
    pub reviewer: String,
    pub review_model: String,
    pub probe_timeout: Duration,
    pub review_timeout: Duration,
}

impl UpdaterConfig {
    /// Production feeds, the smol role for reviews, no trusted keys, no gate adapters.
    pub fn new(root: impl Into<PathBuf>, products: Vec<ProductSpec>) -> Self {
        Self {
            root: root.into(),
            products,
            github_api: "https://api.github.com".to_owned(),
            npm_registry: "https://registry.npmjs.org".to_owned(),
            github_token: None,
            trusted_keys: Vec::new(),
            ports: None,
            reviewer: "omp".to_owned(),
            review_model: "@smol".to_owned(),
            probe_timeout: Duration::from_secs(60),
            review_timeout: Duration::from_secs(180),
        }
    }
}

/// Release asset suffix of this platform as omp names its binaries (`linux-x64`, `darwin-arm64`).
pub fn platform() -> String {
    let os = if std::env::consts::OS == "macos" { "darwin" } else { std::env::consts::OS };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// `(id, label, feed, asset, executable)`; `gh:` feeds are `owner/repo`, `npm:` a package.
/// `app` is the running app, `omp` gets every gate, the rest are harnesses with a changelog gate.
type CatalogRow = (&'static str, &'static str, &'static str, Option<&'static str>, Option<&'static str>);

const CATALOG: &[CatalogRow] = &[
    ("app", "loa", "gh:MrGreenGaming14/omp-gui", Some("loa-app-{platform}.tar.gz"), Some("loa-app")),
    ("omp", "omp", "gh:can1357/oh-my-pi", Some("omp-{platform}"), None),
    ("claude", "Claude Code", "npm:@anthropic-ai/claude-code", None, None),
    ("codex", "Codex", "npm:@openai/codex", None, None),
    ("opencode", "OpenCode", "npm:opencode-ai", None, None),
];

/// The products the app watches: itself, omp and the harnesses its adapters drive.
pub fn default_products(app_version: Version) -> Vec<ProductSpec> {
    let platform = platform();
    let spec = |&(id, label, feed, asset, executable): &CatalogRow| {
        let feed = match feed.split_once(':') {
            Some(("gh", repo)) => {
                let (owner, repo) = repo.split_once('/').unwrap_or_default();
                Feed::GitHub { owner: owner.into(), repo: repo.into() }
            }
            _ => Feed::Npm { package: feed.trim_start_matches("npm:").into() },
        };
        let (kind, installed, gates) = match id {
            "app" => (ProductKind::App, Installed::Running(app_version.clone()), Gates::None),
            "omp" => (ProductKind::Omp, Installed::Executable(id.into()), Gates::Omp(OmpGates::default())),
            _ => (ProductKind::Harness(id.into()), Installed::Executable(id.into()), Gates::Changelog),
        };
        let asset = asset.map(|a| a.replace("{platform}", &platform));
        let product = Product { id: id.into(), kind, feed, prereleases: PrereleasePolicy::Stable, asset };
        ProductSpec { product, label: label.into(), installed, executable: executable.map(Into::into), gates }
    };
    CATALOG.iter().map(spec).collect()
}

/// Counts running turns; swaps wait until none runs.
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

/// The version a product uses now and its executable.
struct Current {
    version: Version,
    executable: Option<PathBuf>,
}

/// What looking for a product's executable found.
type Detected = std::result::Result<(Version, PathBuf), String>;

struct Inner {
    config: UpdaterConfig,
    /// Feed sources by [`Feed::kind`].
    feeds: BTreeMap<&'static str, Arc<dyn FeedSource>>,
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
    /// Opens the updater under `config.root`, loading state and feed cache. Nothing is fetched.
    pub fn open(config: UpdaterConfig) -> Result<Self> {
        let client = feeds::client()?;
        let github = feeds::GitHub { client: client.clone(), api: config.github_api.clone(), token: config.github_token.clone() };
        let npm = feeds::Npm { client: client.clone(), registry: config.npm_registry.clone() };
        let feeds: BTreeMap<&'static str, Arc<dyn FeedSource>> = BTreeMap::from([("github", Arc::new(github) as _), ("npm", Arc::new(npm) as _)]);
        let mut swapper = Swapper::new(&config.root);
        if let Some(app) = config.products.iter().find(|p| p.product.kind == ProductKind::App) {
            swapper.app_product = app.product.id.clone();
        }
        let cache_path = config.root.join("feeds.json");
        // The cache only saves requests; a damaged one is dropped.
        let cache = match crate::read_json(&cache_path) {
            Err(Error::Json { .. }) => None,
            other => other?,
        };
        let inner = Inner {
            feeds,
            stager: Stager::new(client, &config.root),
            swapper,
            state: Mutex::new(UpdaterState::load(&config.root)?),
            cache: Mutex::new(cache.unwrap_or_default()),
            installed: Mutex::default(),
            signatures: Mutex::default(),
            errors: Mutex::default(),
            rate_limit: Mutex::default(),
            config,
        };
        Ok(Self { inner: Arc::new(inner) })
    }

    pub fn config(&self) -> &UpdaterConfig {
        &self.inner.config
    }

    fn spec(&self, id: &str) -> Result<&ProductSpec> {
        self.inner.config.products.iter().find(|p| p.product.id == id).ok_or_else(|| Error::UnknownProduct(id.to_owned()))
    }

    /// Keeps a step's error for the screen; a success clears it.
    fn note<T>(&self, id: &str, result: Result<T>) -> Result<T> {
        let mut errors = self.inner.errors.locked();
        match &result {
            Ok(_) => errors.remove(id),
            Err(e) => errors.insert(id.to_owned(), e.to_string()),
        };
        result
    }

    fn save_state(&self, state: &UpdaterState) -> Result<()> {
        state.save(&self.inner.config.root)
    }

    /// Finds every product's executable and asks it for its version.
    pub async fn detect(&self) {
        let timeout = self.inner.config.probe_timeout;
        let probes = self.inner.config.products.iter().filter_map(|spec| {
            let Installed::Executable(name) = &spec.installed else { return None };
            Some(async move {
                let found = match loa_util::exec::which(name) {
                    Some(path) => executable_version(&path, timeout).await.map(|v| (v, path)),
                    None => Err(format!("{} not found", name.display())),
                };
                (spec.product.id.clone(), found)
            })
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
            let executable = staged_executable(spec, &self.staged(id, &active.current)?).ok();
            return Ok(Some(Current { version: active.current, executable }));
        }
        let installed = self.inner.installed.locked();
        Ok(installed.get(id).and_then(|found| found.as_ref().ok()).map(|(v, path)| Current { version: v.clone(), executable: Some(path.clone()) }))
    }

    /// The executable new sessions of `id` start: the active staged version, else the system one.
    pub fn executable(&self, id: &str) -> Option<PathBuf> {
        self.current(self.spec(id).ok()?).ok().flatten()?.executable
    }

    fn staged(&self, id: &str, version: &Version) -> Result<Staged> {
        stage::staged(&self.inner.config.root, id, version)?.ok_or_else(|| Error::NotStaged { product: id.to_owned(), version: version.clone() })
    }

    /// The current version, detecting executables first when needed.
    async fn current_or_detect(&self, spec: &ProductSpec) -> Result<Option<Current>> {
        if !self.inner.installed.locked().contains_key(&spec.product.id) && matches!(spec.installed, Installed::Executable(_)) {
            if let Some(current) = self.current(spec)? {
                return Ok(Some(current));
            }
            self.detect().await;
        }
        self.current(spec)
    }

    /// The release to offer over `current`, from the cache, after pins and waits.
    fn offer(&self, spec: &ProductSpec, current: &Version) -> Option<Release> {
        let id = &spec.product.id;
        let cache = self.inner.cache.locked();
        let state = self.inner.state.locked();
        let allowed: Vec<Release> =
            cache.get(&feeds::cache_key(&spec.product))?.releases.iter().filter(|r| decision::offer_allowed(&state, id, &r.version)).cloned().collect();
        feeds::newest(&allowed, current, spec.product.prereleases, state.pins.get(id)).cloned()
    }

    /// The offer over the version in use (0.0.0 when nothing is installed).
    async fn offered(&self, spec: &ProductSpec) -> Result<Release> {
        let current = self.current_or_detect(spec).await?.map_or_else(|| Version::new(0, 0, 0), |c| c.version);
        self.offer(spec, &current).ok_or_else(|| Error::NothingOffered(spec.product.id.clone()))
    }

    /// Reads `id`'s feed conditionally and returns the release to offer, if any.
    pub async fn check(&self, id: &str) -> Result<Option<Release>> {
        let result = async {
            let spec = self.spec(id)?;
            let key = feeds::cache_key(&spec.product);
            let source = self.inner.feeds.get(spec.product.feed.kind()).ok_or_else(|| Error::Feed(format!("no source for {key}")))?;
            let cached = self.inner.cache.locked().get(&key).cloned();
            match source.fetch(&spec.product, cached.as_ref()).await? {
                FeedResponse::NotModified if cached.is_none() => return Err(Error::Feed(format!("{key} answered 304 without a cached response"))),
                FeedResponse::NotModified => {}
                FeedResponse::Fresh { feed, rate_limit } => {
                    if rate_limit.is_some() {
                        *self.inner.rate_limit.locked() = rate_limit;
                    }
                    let mut cache = self.inner.cache.locked();
                    cache.insert(key, feed);
                    crate::write_json(&self.inner.config.root.join("feeds.json"), &*cache)?;
                }
            }
            {
                let mut state = self.inner.state.locked();
                state.last_check_unix.insert(id.to_owned(), now_secs());
                self.save_state(&state)?;
            }
            match self.offered(spec).await {
                Err(Error::NothingOffered(_)) => Ok(None),
                other => other.map(Some),
            }
        }
        .await;
        self.note(id, result)
    }

    /// Checks every product's feed.
    pub async fn check_all(&self) -> Vec<(String, Result<Option<Release>>)> {
        let checks = self.inner.config.products.iter().map(|spec| async move { (spec.product.id.clone(), self.check(&spec.product.id).await) });
        futures::future::join_all(checks).await
    }

    /// Downloads, verifies and unpacks the offer; app releases have their signature checked now.
    pub async fn stage(&self, id: &str) -> Result<Version> {
        let result = async {
            let spec = self.spec(id)?;
            let release = self.offered(spec).await?;
            let staged = self.inner.stager.stage(&release).await?;
            if spec.product.kind == ProductKind::App {
                self.verify_staged(id, staged).await;
            }
            Ok(release.version)
        }
        .await;
        self.note(id, result)
    }

    /// Checks the signature of the staged offer of `id`.
    pub async fn verify(&self, id: &str) -> Result<SignatureStatus> {
        let release = self.offered(self.spec(id)?).await?;
        Ok(self.verify_staged(id, self.staged(id, &release.version)?).await)
    }

    async fn verify_staged(&self, id: &str, staged: Staged) -> SignatureStatus {
        let version = staged.manifest.version.clone();
        let keys = self.inner.config.trusted_keys.clone();
        let status = if keys.is_empty() {
            SignatureStatus::NoTrustedKeys
        } else {
            // Hashing a large artifact blocks.
            match tokio::task::spawn_blocking(move || crate::signature::verify_staged(&staged, &keys)).await {
                Ok(Ok(())) => SignatureStatus::Verified,
                Ok(Err(Error::Unsigned(_))) => SignatureStatus::Unsigned,
                Ok(Err(e)) => SignatureStatus::Rejected(e.to_string()),
                Err(e) => SignatureStatus::Rejected(format!("verification task failed: {e}")),
            }
        };
        self.inner.signatures.locked().insert((id.to_owned(), version), status.clone());
        status
    }

    /// Gates the staged offer against the version in use and records swap or hold.
    pub async fn gate(&self, id: &str) -> Result<GateReport> {
        let result = async {
            let spec = self.spec(id)?;
            let not_installed = || Error::NotInstalled { product: id.to_owned(), reason: self.missing_reason(id) };
            let current = self.current_or_detect(spec).await?.ok_or_else(not_installed)?;
            let release = self.offer(spec, &current.version).ok_or_else(|| Error::NothingOffered(id.to_owned()))?;
            let staged = self.staged(id, &release.version)?;
            let report = match &spec.gates {
                Gates::None => GateReport { product: id.to_owned(), from: current.version.clone(), to: release.version, checks: Vec::new() },
                gates => self.run_checks(spec, gates, &current, &staged_executable(spec, &staged)?, &release.version).await,
            };
            let mut state = self.inner.state.locked();
            decision::record(&mut state, &report, now_secs());
            state.reports.insert(id.to_owned(), report.clone());
            self.save_state(&state)?;
            Ok(report)
        }
        .await;
        self.note(id, result)
    }

    fn missing_reason(&self, id: &str) -> String {
        match self.inner.installed.locked().get(id) {
            Some(Err(reason)) => reason.clone(),
            _ => "no version found".to_owned(),
        }
    }

    async fn run_checks(&self, spec: &ProductSpec, gates: &Gates, current: &Current, new: &Path, to: &Version) -> GateReport {
        let config = &self.inner.config;
        let id = &spec.product.id;
        let report = |checks| GateReport { product: id.clone(), from: current.version.clone(), to: to.clone(), checks };
        let notes = self.changelog(spec, &current.version, to);
        // Reviews run on the omp in use, never on an unchecked staged one.
        let reviewer_exe = if *id == config.reviewer { current.executable.clone() } else { self.executable(&config.reviewer) };
        let ports = config.ports.as_deref();
        let reviewer = ports.zip(reviewer_exe).map(|(p, exe)| p.reviewer(&exe, &config.review_model, config.review_timeout));
        let note = match (notes.trim().is_empty(), &reviewer) {
            (true, _) => Some(CheckResult::pass(GateCheck::Changelog, vec![format!("no release notes published after {}", current.version)])),
            (false, None) => Some(CheckResult::failed_to_run(GateCheck::Changelog, format!("{} is not installed to review it", config.reviewer))),
            (false, Some(_)) => None,
        };
        let changelog = reviewer.as_deref().filter(|_| note.is_none()).map(|r| (r, notes.as_str()));
        let old = current.executable.clone().unwrap_or_default();
        let probe = match (gates, ports) {
            (Gates::Omp(_), _) if current.executable.is_none() => {
                return report(vec![CheckResult::failed_to_run(GateCheck::ToolSchemas, "the version in use has no executable to compare with")]);
            }
            (Gates::Omp(_), None) => return report(vec![CheckResult::failed_to_run(GateCheck::ToolSchemas, "no gate adapters are configured")]),
            (Gates::Omp(omp), Some(ports)) => Some((ports.probe(omp, config.probe_timeout), omp)),
            _ => None,
        };
        let inputs = GateInputs { old_executable: &old, new_executable: new, omp: probe.as_ref().map(|(p, g)| (&**p, *g)), changelog };
        let mut report = gates::run_gates(id, &current.version, to, &inputs).await;
        report.checks.extend(note);
        report
    }

    /// Release notes in `(from, to]`, oldest first.
    fn changelog(&self, spec: &ProductSpec, from: &Version, to: &Version) -> String {
        let cache = self.inner.cache.locked();
        let Some(feed) = cache.get(&feeds::cache_key(&spec.product)) else { return String::new() };
        let notes: Vec<(Version, &str)> =
            feeds::between(&feed.releases, from, to).into_iter().filter(|r| !r.notes.trim().is_empty()).map(|r| (r.version.clone(), r.notes.as_str())).collect();
        if notes.is_empty() { String::new() } else { gates::changelog_text(&notes) }
    }

    /// Applies the user's answer to `id`'s hold; returns the version now pending swap.
    pub fn choose(&self, id: &str, choice: HoldChoice) -> Result<Option<Version>> {
        let result = (|| {
            let current = self.current(self.spec(id)?)?.ok_or_else(|| Error::NotInstalled { product: id.to_owned(), reason: "no version in use".into() })?;
            let mut state = self.inner.state.locked();
            let to = decision::apply_choice(&mut state, id, &current.version, choice);
            self.save_state(&state)?;
            Ok(to)
        })();
        self.note(id, result)
    }

    /// Removes `id`'s pin and wait.
    pub fn unpin(&self, id: &str) -> Result<()> {
        self.spec(id)?;
        let mut state = self.inner.state.locked();
        state.pins.remove(id);
        state.waits.remove(id);
        self.save_state(&state)
    }

    /// Replaces every pin, e.g. with the config file's. Unknown products are refused.
    pub fn set_pins(&self, pins: BTreeMap<String, Version>) -> Result<()> {
        pins.keys().try_for_each(|id| self.spec(id).map(drop))?;
        let mut state = self.inner.state.locked();
        if state.pins != pins {
            state.pins = pins;
            self.save_state(&state)?;
        }
        Ok(())
    }

    /// Swaps in every cleared version unless a turn runs.
    pub fn apply_pending(&self, activity: &dyn Activity) -> Vec<(String, Result<SwapOutcome>)> {
        let pending: Vec<String> = self.inner.state.locked().pending_swap.keys().cloned().collect();
        pending.into_iter().map(|id| (id.clone(), self.swap(&id, activity))).collect()
    }

    fn trusted(&self) -> Option<&[VerifyingKey]> {
        Some(self.inner.config.trusted_keys.as_slice()).filter(|k| !k.is_empty())
    }

    /// Swaps in `id`'s pending version unless a turn runs; a failed swap is dropped from pending.
    pub fn swap(&self, id: &str, activity: &dyn Activity) -> Result<SwapOutcome> {
        let result = (|| {
            self.spec(id)?;
            let to = self.inner.state.locked().pending_swap.get(id).cloned().ok_or_else(|| Error::NothingOffered(id.to_owned()))?;
            let outcome = self.inner.swapper.try_swap(id, &to, self.trusted(), activity);
            if !matches!(outcome, Ok(SwapOutcome::Deferred)) {
                let mut state = self.inner.state.locked();
                state.pending_swap.remove(id);
                self.save_state(&state)?;
            }
            outcome
        })();
        self.note(id, result)
    }

    /// Makes the previous version of `id` current again, unless a turn runs.
    pub fn rollback(&self, id: &str, activity: &dyn Activity) -> Result<SwapOutcome> {
        let result = (|| {
            self.spec(id)?;
            let outcome = self.inner.swapper.rollback(id, self.trusted(), activity)?;
            if matches!(outcome, SwapOutcome::Swapped { .. }) {
                let mut state = self.inner.state.locked();
                state.pending_swap.remove(id);
                self.save_state(&state)?;
            }
            Ok(outcome)
        })();
        self.note(id, result)
    }

    /// A snapshot of every product; runs nothing.
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
            (_, Some(_)) => current.as_ref().and_then(|c| c.executable.clone()).map_or_else(|| InstalledStatus::Missing("the active version has no executable".into()), InstalledStatus::Managed),
            (Installed::Executable(_), None) => match self.inner.installed.locked().get(&id) {
                None => InstalledStatus::Unknown,
                Some(Ok((_, path))) => InstalledStatus::System(path.clone()),
                Some(Err(reason)) => InstalledStatus::Missing(reason.clone()),
            },
        };
        let version = current.as_ref().map_or_else(|| Version::new(0, 0, 0), |c| c.version.clone());
        let offer = self.offer(spec, &version);
        let include = spec.product.prereleases == PrereleasePolicy::Include;
        let newest = self.inner.cache.locked().get(&feeds::cache_key(&spec.product)).and_then(|feed| {
            feed.releases.iter().filter(|r| include || r.version.pre.is_empty()).map(|r| r.version.clone()).max()
        });
        let changelog = offer.as_ref().map(|o| self.changelog(spec, &version, &o.version)).unwrap_or_default();
        let signature = offer.as_ref().and_then(|o| self.inner.signatures.locked().get(&(id.clone(), o.version.clone())).cloned());
        let restart_required = matches!(&spec.installed, Installed::Running(running) if active.as_ref().is_some_and(|a| a.current != *running));
        let source = match &spec.product.feed {
            Feed::GitHub { owner, repo } => format!("GitHub {owner}/{repo}"),
            Feed::Npm { package } => format!("npm {package}"),
        };
        let state = self.inner.state.locked();
        ProductStatus {
            label: spec.label.clone(),
            kind: spec.product.kind.clone(),
            source,
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
}

/// Runs `<executable> --version` and reads the first semantic version it prints.
pub async fn executable_version(executable: &Path, timeout: Duration) -> std::result::Result<Version, String> {
    let text = loa_util::exec::probe_cli(executable, &["--version"], timeout).await.map_err(|e| e.to_string())?;
    loa_util::exec::parse_version(&text).and_then(|v| Version::parse(&v).ok()).ok_or_else(|| format!("no version in {:?}", text.trim()))
}

/// Fully staged versions of `product`, ascending.
fn staged_versions(root: &Path, product: &str) -> Vec<Version> {
    let Ok(entries) = std::fs::read_dir(stage::product_dir(root, product)) else { return Vec::new() };
    let mut versions: Vec<Version> = entries
        .flatten()
        .filter(|e| e.path().join(MANIFEST_FILE).is_file())
        .filter_map(|e| Version::parse(e.file_name().to_str()?).ok())
        .collect();
    versions.sort();
    versions
}

/// The executable inside a staged payload: the configured pattern, the single file, or npm's `bin`.
fn staged_executable(spec: &ProductSpec, staged: &Staged) -> Result<PathBuf> {
    let version = &staged.manifest.version;
    let payload = staged.payload();
    let none = |reason: String| Error::NoExecutable { product: spec.product.id.clone(), version: version.clone(), reason };
    let path = match &spec.executable {
        Some(pattern) => payload.join(pattern.replace("{version}", &version.to_string())),
        None if payload.join(&staged.manifest.artifact_name).is_file() => payload.join(&staged.manifest.artifact_name),
        None if payload.join("package.json").is_file() => payload.join(npm_bin(&payload.join("package.json"), &spec.product.id).map_err(none)?),
        None => return Err(none("the payload is neither a single file nor an npm package".into())),
    };
    if !path.starts_with(&payload) || !path.is_file() {
        return Err(none(format!("{} is not a file in the payload", path.display())));
    }
    Ok(path)
}

/// The `bin` of an npm `package.json`: the string form, the entry named like the product, or the only one.
fn npm_bin(package: &Path, product: &str) -> std::result::Result<String, String> {
    let text = std::fs::read_to_string(package).map_err(|e| format!("reading {}: {e}", package.display()))?;
    let json: serde_json::Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", package.display()))?;
    let bin = match json.get("bin") {
        Some(serde_json::Value::String(path)) => Some(path.as_str()),
        Some(serde_json::Value::Object(map)) => map.get(product).or_else(|| map.values().next().filter(|_| map.len() == 1)).and_then(|v| v.as_str()),
        _ => None,
    };
    let bin = bin.ok_or("package.json names no executable")?;
    if bin.split(['/', '\\']).any(|part| part == "..") {
        return Err(format!("package.json bin {bin:?} leaves the package"));
    }
    Ok(bin.to_owned())
}
