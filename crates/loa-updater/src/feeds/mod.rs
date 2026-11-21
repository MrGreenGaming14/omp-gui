//! Release feeds: GitHub releases and npm packages.
//!
//! Each feed turns its upstream listing into [`Release`]s, using conditional
//! requests (`If-None-Match`) so an unchanged feed costs nothing. [`newest`]
//! picks what to offer; [`Watcher`] ties both together per [`Product`].

mod github;
mod npm;

pub use github::GitHubFeed;
pub use npm::NpmFeed;

use loa_util::time::now_secs;
use crate::product::{CachedFeed, Feed, PrereleasePolicy, Product, Release};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Errors fetching or reading a release feed.
#[derive(Debug, thiserror::Error)]
pub enum FeedError {
    #[error("request to {url} failed: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url} answered HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url} returned malformed JSON: {source}")]
    Json {
        url: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("rate limited until unix time {retry_at_unix}")]
    RateLimited { retry_at_unix: u64 },
    #[error("feed reports invalid version {version:?}: {source}")]
    BadVersion {
        version: String,
        #[source]
        source: semver::Error,
    },
    #[error("{url} answered 304 Not Modified but nothing is cached")]
    NotModifiedWithoutCache { url: String },
    #[error("product {product} is not published on this feed")]
    WrongFeed { product: String },
}

pub type Result<T, E = FeedError> = std::result::Result<T, E>;

/// Rate limit state a feed reported with its last response.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RateLimit {
    /// Requests left in the current window.
    pub remaining: u64,
    /// Unix time the window resets.
    pub reset_unix: u64,
}

/// Outcome of fetching a feed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeedResponse {
    /// The feed is unchanged since the cached response; its releases stay
    /// valid.
    NotModified,
    /// A new listing, with its `ETag` for the next conditional request.
    Fresh {
        feed: CachedFeed,
        /// Rate limit state when the feed reports one.
        rate_limit: Option<RateLimit>,
    },
}

/// The release to offer: the highest version strictly above `current` that
/// `policy` allows. With a `pin`, only the pinned version is ever offered,
/// and only when it differs from `current` and the feed lists it.
pub fn newest<'a>(
    releases: &'a [Release],
    current: &Version,
    policy: PrereleasePolicy,
    pin: Option<&Version>,
) -> Option<&'a Release> {
    if let Some(pin) = pin {
        if pin == current {
            return None;
        }
        return releases.iter().find(|r| &r.version == pin);
    }
    releases
        .iter()
        .filter(|r| &r.version > current)
        .filter(|r| policy == PrereleasePolicy::Include || r.version.pre.is_empty())
        .max_by(|a, b| a.version.cmp(&b.version))
}

/// Releases with versions in `(from, to]`, ascending, for changelog text.
pub fn between<'a>(releases: &'a [Release], from: &Version, to: &Version) -> Vec<&'a Release> {
    let mut out: Vec<&Release> =
        releases.iter().filter(|r| &r.version > from && &r.version <= to).collect();
    out.sort_by(|a, b| a.version.cmp(&b.version));
    out
}

/// Key of `product` in [`Watcher::cache`]. Covers every product setting
/// that changes what a feed returns, so editing one starts a fresh fetch
/// instead of trusting a 304 for the old settings.
pub fn cache_key(product: &Product) -> String {
    let policy = match product.prereleases {
        PrereleasePolicy::Stable => "stable",
        PrereleasePolicy::Include => "include",
    };
    let source = match &product.feed {
        Feed::GitHub { owner, repo } => format!("github:{owner}/{repo}"),
        Feed::Npm { package } => format!("npm:{package}"),
    };
    format!("{}|{source}|{policy}|{}", product.id, product.asset.as_deref().unwrap_or(""))
}

/// Watches the feeds of every product, keeping the last response of each
/// for conditional requests.
pub struct Watcher {
    pub github: GitHubFeed,
    pub npm: NpmFeed,
    /// Token for GitHub API requests; raises the rate limit.
    pub github_token: Option<String>,
    /// Last response per [`cache_key`]. Persist it to keep conditional
    /// requests working across restarts.
    pub cache: BTreeMap<String, CachedFeed>,
    /// GitHub rate limit state from the last fresh response.
    pub github_rate_limit: Option<RateLimit>,
}

impl Watcher {
    pub fn new(
        github: GitHubFeed,
        npm: NpmFeed,
        github_token: Option<String>,
        cache: BTreeMap<String, CachedFeed>,
    ) -> Self {
        Self { github, npm, github_token, cache, github_rate_limit: None }
    }

    /// Fetches `product`'s feed, updates the cache and returns the release
    /// to offer over `current` (see [`newest`]).
    pub async fn check(
        &mut self,
        product: &Product,
        current: &Version,
        pin: Option<&Version>,
    ) -> Result<Option<Release>> {
        let key = cache_key(product);
        let cached = self.cache.get(&key);
        let response = match &product.feed {
            Feed::GitHub { .. } => {
                self.github.fetch(product, cached, self.github_token.as_deref()).await?
            }
            Feed::Npm { .. } => self.npm.fetch(product, cached).await?,
        };
        match response {
            FeedResponse::NotModified => match self.cache.get(&key) {
                Some(feed) => Ok(newest(&feed.releases, current, product.prereleases, pin).cloned()),
                None => Err(FeedError::NotModifiedWithoutCache { url: describe(product) }),
            },
            FeedResponse::Fresh { feed, rate_limit } => {
                if matches!(product.feed, Feed::GitHub { .. }) && rate_limit.is_some() {
                    self.github_rate_limit = rate_limit;
                }
                let offer = newest(&feed.releases, current, product.prereleases, pin).cloned();
                self.cache.insert(key, feed);
                Ok(offer)
            }
        }
    }
}

/// Names a product's feed in errors that have no single URL.
fn describe(product: &Product) -> String {
    match &product.feed {
        Feed::GitHub { owner, repo } => format!("github.com/{owner}/{repo}"),
        Feed::Npm { package } => format!("npm package {package}"),
    }
}

/// `ETag` of a response, as sent back in `If-None-Match`.
fn etag(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Reads a successful response body as JSON.
async fn read_json<T: serde::de::DeserializeOwned>(
    url: &str,
    response: reqwest::Response,
) -> Result<T> {
    let body = response
        .bytes()
        .await
        .map_err(|source| FeedError::Http { url: url.to_owned(), source })?;
    serde_json::from_slice(&body).map_err(|source| FeedError::Json { url: url.to_owned(), source })
}

/// Error for a response that is neither success nor 304. A 403 or 429
/// that says the rate limit is exhausted (`x-ratelimit-remaining: 0`) or
/// asks to retry later (`retry-after`, in seconds) becomes
/// [`FeedError::RateLimited`].
fn reject(url: &str, response: &reqwest::Response) -> FeedError {
    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let headers = response.headers();
        let remaining = header_u64(headers, "x-ratelimit-remaining");
        let reset = header_u64(headers, "x-ratelimit-reset");
        let retry_after = header_u64(headers, "retry-after");
        let retry_at = match (remaining, reset, retry_after) {
            (Some(0), Some(reset), _) => Some(reset),
            (_, _, Some(secs)) => Some(now_secs().saturating_add(secs)),
            // Exhausted without a reset time: GitHub asks clients to wait at
            // least a minute.
            (Some(0), None, None) => Some(now_secs().saturating_add(60)),
            _ => None,
        };
        if let Some(retry_at_unix) = retry_at {
            return FeedError::RateLimited { retry_at_unix };
        }
    }
    FeedError::Status { url: url.to_owned(), status: status.as_u16() }
}

/// Rate limit state from `x-ratelimit-remaining` and `x-ratelimit-reset`.
fn rate_limit(response: &reqwest::Response) -> Option<RateLimit> {
    let headers = response.headers();
    Some(RateLimit {
        remaining: header_u64(headers, "x-ratelimit-remaining")?,
        reset_unix: header_u64(headers, "x-ratelimit-reset")?,
    })
}

/// A header holding a non-negative integer.
fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(v: &str) -> Release {
        Release {
            product: "omp".into(),
            version: Version::parse(v).unwrap(),
            notes: String::new(),
            published_at: None,
            artifact: None,
        }
    }

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn newest_respects_policy_and_pin() {
        let rs = vec![release("1.0.0"), release("1.2.0"), release("1.3.0-beta.1"), release("1.1.0")];
        let pick = |current: &str, policy, pin: Option<&Version>| {
            newest(&rs, &v(current), policy, pin).map(|r| r.version.to_string())
        };
        assert_eq!(pick("1.0.0", PrereleasePolicy::Stable, None).as_deref(), Some("1.2.0"));
        assert_eq!(
            pick("1.0.0", PrereleasePolicy::Include, None).as_deref(),
            Some("1.3.0-beta.1")
        );
        assert_eq!(pick("1.2.0", PrereleasePolicy::Stable, None), None);
        let pin = v("1.1.0");
        assert_eq!(pick("1.0.0", PrereleasePolicy::Stable, Some(&pin)).as_deref(), Some("1.1.0"));
        // A pin below current is a deliberate downgrade.
        assert_eq!(pick("1.2.0", PrereleasePolicy::Stable, Some(&pin)).as_deref(), Some("1.1.0"));
        assert_eq!(pick("1.1.0", PrereleasePolicy::Stable, Some(&pin)), None);
        let missing = v("9.0.0");
        assert_eq!(pick("1.0.0", PrereleasePolicy::Include, Some(&missing)), None);
    }

    #[test]
    fn between_is_half_open_and_ascending() {
        let rs = vec![release("1.3.0"), release("1.0.0"), release("1.2.0"), release("1.1.0")];
        let got: Vec<String> =
            between(&rs, &v("1.0.0"), &v("1.2.0")).iter().map(|r| r.version.to_string()).collect();
        assert_eq!(got, ["1.1.0", "1.2.0"]);
    }
}
