//! Release feeds behind the [`FeedSource`] port: GitHub releases and npm packages, read with
//! conditional requests so an unchanged feed costs nothing.

use crate::model::{CachedFeed, Feed, PrereleasePolicy, Product};
use loa_settings::updates::{ArchiveFormat, Artifact, Checksum, RateLimit, Release};
use crate::{Error, Result};
use async_trait::async_trait;
use base64::Engine as _;
use loa_util::time::now_secs;
use reqwest::header::{ACCEPT, ETAG, HeaderMap, IF_NONE_MATCH};
use reqwest::{RequestBuilder, StatusCode};
use semver::Version;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

/// Outcome of fetching a feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedResponse {
    /// Unchanged since the cached response.
    NotModified,
    Fresh { feed: CachedFeed, rate_limit: Option<RateLimit> },
}

/// Lists the releases of a product from where they are published.
#[async_trait]
pub trait FeedSource: Send + Sync {
    async fn fetch(&self, product: &Product, cached: Option<&CachedFeed>) -> Result<FeedResponse>;
}

/// An HTTP client on rustls with the Mozilla roots; it follows redirects to release CDNs.
pub fn client() -> Result<reqwest::Client> {
    let failed = |e: &dyn std::fmt::Display| Error::Http { url: String::new(), reason: format!("building the HTTP client: {e}") };
    let roots = rustls::RootCertStore { roots: webpki_roots::TLS_SERVER_ROOTS.to_vec() };
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|e| failed(&e))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls)
        .user_agent(concat!("loa-updater/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(15))
        .read_timeout(Duration::from_secs(60))
        .build()
        .map_err(|e| failed(&e))
}

/// The release to offer: the highest allowed version above `current`, or only the pin when one is set.
pub fn newest<'a>(releases: &'a [Release], current: &Version, policy: PrereleasePolicy, pin: Option<&Version>) -> Option<&'a Release> {
    if let Some(pin) = pin {
        return releases.iter().find(|r| &r.version == pin && pin != current);
    }
    let allowed = |r: &&Release| &r.version > current && (policy == PrereleasePolicy::Include || r.version.pre.is_empty());
    releases.iter().filter(allowed).max_by(|a, b| a.version.cmp(&b.version))
}

/// Releases in `(from, to]`, ascending.
pub fn between<'a>(releases: &'a [Release], from: &Version, to: &Version) -> Vec<&'a Release> {
    let mut out: Vec<&Release> = releases.iter().filter(|r| &r.version > from && &r.version <= to).collect();
    out.sort_by(|a, b| a.version.cmp(&b.version));
    out
}

/// Feed cache key covering every setting that changes what a feed returns.
pub fn cache_key(product: &Product) -> String {
    let policy = if product.prereleases == PrereleasePolicy::Stable { "stable" } else { "include" };
    let source = match &product.feed {
        Feed::GitHub { owner, repo } => format!("github:{owner}/{repo}"),
        Feed::Npm { package } => format!("npm:{package}"),
    };
    format!("{}|{source}|{policy}|{}", product.id, product.asset.as_deref().unwrap_or(""))
}

/// A listing with its ETag and rate limit, or `None` when the feed answered 304.
type Listing<T> = Option<(T, Option<String>, Option<RateLimit>)>;

/// Sends `request` with the cached ETag and reads a JSON answer.
async fn conditional<T: DeserializeOwned>(request: RequestBuilder, url: &str, cached: Option<&CachedFeed>) -> Result<Listing<T>> {
    let http = |e: reqwest::Error| Error::Http { url: url.to_owned(), reason: e.to_string() };
    let request = match cached.and_then(|c| c.etag.as_deref()) {
        Some(tag) => request.header(IF_NONE_MATCH, tag),
        None => request,
    };
    let response = request.send().await.map_err(http)?;
    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(None);
    }
    let headers = response.headers().clone();
    if !response.status().is_success() {
        return Err(reject(url, response.status(), &headers));
    }
    let body = response.bytes().await.map_err(http)?;
    let value = serde_json::from_slice(&body).map_err(|e| Error::Feed(format!("{url} returned malformed JSON: {e}")))?;
    let etag = headers.get(ETAG).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let rate_limit = header(&headers, "x-ratelimit-remaining").zip(header(&headers, "x-ratelimit-reset"));
    Ok(Some((value, etag, rate_limit.map(|(remaining, reset_unix)| RateLimit { remaining, reset_unix }))))
}

/// A 403 or 429 with an exhausted limit or a `retry-after` is [`Error::RateLimited`]; anything else is a status error.
fn reject(url: &str, status: StatusCode, headers: &HeaderMap) -> Error {
    if matches!(status, StatusCode::FORBIDDEN | StatusCode::TOO_MANY_REQUESTS) {
        let retry_at = match (header(headers, "x-ratelimit-remaining"), header(headers, "x-ratelimit-reset"), header(headers, "retry-after")) {
            (Some(0), Some(reset), _) => Some(reset),
            (_, _, Some(secs)) => Some(now_secs().saturating_add(secs)),
            // Exhausted without a reset time: GitHub asks clients to wait at least a minute.
            (Some(0), None, None) => Some(now_secs().saturating_add(60)),
            _ => None,
        };
        if let Some(at) = retry_at {
            return Error::RateLimited(at);
        }
    }
    Error::Status { url: url.to_owned(), status: status.as_u16() }
}

fn header(headers: &HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

fn wrong_feed(product: &Product) -> Error {
    Error::Feed(format!("{} is not published on this feed", product.id))
}

/// GitHub releases through the REST API (`https://api.github.com` in production).
#[derive(Clone, Debug)]
pub struct GitHub {
    pub client: reqwest::Client,
    pub api: String,
    /// Raises GitHub's rate limit when set.
    pub token: Option<String>,
}

#[derive(Deserialize)]
struct GhRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GhAsset>,
}

#[derive(Deserialize)]
struct GhAsset {
    name: String,
    browser_download_url: String,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    digest: Option<String>,
}

#[async_trait]
impl FeedSource for GitHub {
    /// Leaves out drafts, non-semver tags and, for stable products, prereleases.
    async fn fetch(&self, product: &Product, cached: Option<&CachedFeed>) -> Result<FeedResponse> {
        let Feed::GitHub { owner, repo } = &product.feed else { return Err(wrong_feed(product)) };
        let url = format!("{}/repos/{owner}/{repo}/releases?per_page=100", self.api.trim_end_matches('/'));
        let mut request = self.client.get(&url).header(ACCEPT, "application/vnd.github+json").header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }
        let Some((listed, etag, rate_limit)) = conditional::<Vec<GhRelease>>(request, &url, cached).await? else {
            return Ok(FeedResponse::NotModified);
        };
        let mut releases = Vec::new();
        for release in listed {
            let tag = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
            let stable_only = product.prereleases == PrereleasePolicy::Stable;
            let Some(version) = Version::parse(tag).ok().filter(|_| !release.draft && !(release.prerelease && stable_only)) else { continue };
            let artifact = match &product.asset {
                Some(pattern) => self.artifact(&pattern.replace("{version}", &version.to_string()), &release.assets).await?,
                None => None,
            };
            let notes = release.body.unwrap_or_default();
            releases.push(Release { product: product.id.clone(), version, notes, published_at: release.published_at, artifact });
        }
        Ok(FeedResponse::Fresh { feed: CachedFeed { etag, releases }, rate_limit })
    }
}

impl GitHub {
    /// The asset `name` with its checksum (asset digest or `.sha256` sidecar) and `.sig` URL.
    async fn artifact(&self, name: &str, assets: &[GhAsset]) -> Result<Option<Artifact>> {
        let find = |name: &str| assets.iter().find(|a| a.name == name);
        let Some(asset) = find(name) else { return Ok(None) };
        let checksum = match asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")).and_then(sha256_hex) {
            Some(hex) => Some(hex),
            None => match find(&format!("{name}.sha256")) {
                Some(sidecar) => self.sidecar(&sidecar.browser_download_url).await?,
                None => None,
            },
        };
        Ok(Some(Artifact {
            name: name.to_owned(),
            url: asset.browser_download_url.clone(),
            size: asset.size,
            checksum: checksum.map(Checksum::Sha256),
            format: format_of(name),
            signature_url: find(&format!("{name}.sig")).map(|a| a.browser_download_url.clone()),
        }))
    }

    /// The digest in a `.sha256` sidecar (`<hex>` or `<hex>  <file>`); none leaves the artifact unverifiable.
    async fn sidecar(&self, url: &str) -> Result<Option<String>> {
        let http = |e: reqwest::Error| Error::Http { url: url.to_owned(), reason: e.to_string() };
        let response = self.client.get(url).send().await.map_err(http)?;
        if !response.status().is_success() {
            return Err(reject(url, response.status(), &response.headers().clone()));
        }
        Ok(response.text().await.map_err(http)?.split_whitespace().next().and_then(sha256_hex))
    }
}

fn sha256_hex(s: &str) -> Option<String> {
    (s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).then(|| s.to_ascii_lowercase())
}

fn format_of(name: &str) -> ArchiveFormat {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".tar.gz") || lower.ends_with(".tgz") {
        ArchiveFormat::TarGz
    } else if lower.ends_with(".zip") {
        ArchiveFormat::Zip
    } else {
        ArchiveFormat::Raw
    }
}

/// Package versions from an npm registry (`https://registry.npmjs.org` in production).
#[derive(Clone, Debug)]
pub struct Npm {
    pub client: reqwest::Client,
    pub registry: String,
}

/// The full packument; the abbreviated install document lacks `time`.
#[derive(Deserialize)]
struct Packument {
    #[serde(default, rename = "dist-tags")]
    dist_tags: BTreeMap<String, String>,
    #[serde(default)]
    versions: BTreeMap<String, NpmVersion>,
    #[serde(default)]
    time: BTreeMap<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct NpmVersion {
    dist: Dist,
    #[serde(default)]
    deprecated: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Dist {
    tarball: String,
    #[serde(default)]
    integrity: Option<String>,
}

#[async_trait]
impl FeedSource for Npm {
    /// Leaves out deprecated versions and, for stable products, everything above the `latest` dist-tag.
    async fn fetch(&self, product: &Product, cached: Option<&CachedFeed>) -> Result<FeedResponse> {
        let Feed::Npm { package } = &product.feed else { return Err(wrong_feed(product)) };
        let url = format!("{}/{}", self.registry.trim_end_matches('/'), package.replace('/', "%2F"));
        let request = self.client.get(&url).header(ACCEPT, "application/json");
        let Some((packument, etag, _)) = conditional::<Packument>(request, &url, cached).await? else {
            return Ok(FeedResponse::NotModified);
        };
        let cap = match (packument.dist_tags.get("latest"), product.prereleases) {
            (Some(tag), PrereleasePolicy::Stable) => Some(Version::parse(tag).map_err(|e| Error::Feed(format!("invalid version {tag:?}: {e}")))?),
            _ => None,
        };
        let mut releases = Vec::new();
        for (key, entry) in packument.versions {
            let Some(version) = Version::parse(&key).ok().filter(|v| entry.deprecated.is_none() && cap.as_ref().is_none_or(|cap| v <= cap)) else {
                continue;
            };
            let name = entry.dist.tarball.rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("package.tgz").to_owned();
            let checksum = entry.dist.integrity.as_deref().and_then(sha512_hex).map(Checksum::Sha512);
            let published_at = packument.time.get(&key).and_then(|t| t.as_str()).map(str::to_owned);
            let artifact = Artifact { name, url: entry.dist.tarball, size: None, checksum, format: ArchiveFormat::NpmTarball, signature_url: None };
            releases.push(Release { product: product.id.clone(), version, notes: String::new(), published_at, artifact: Some(artifact) });
        }
        Ok(FeedResponse::Fresh { feed: CachedFeed { etag, releases }, rate_limit: None })
    }
}

/// The SHA-512 of an SRI `integrity` string (`sha512-<base64>`), as hex.
fn sha512_hex(integrity: &str) -> Option<String> {
    integrity.split_whitespace().find_map(|part| {
        let b64 = part.strip_prefix("sha512-")?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64.split('?').next().unwrap_or(b64)).ok()?;
        (bytes.len() == 64).then(|| hex::encode(bytes))
    })
}
