//! npm registry feed.

use super::{FeedError, FeedResponse, Result, etag, read_json, reject};
use crate::product::{
    ArchiveFormat, Artifact, CachedFeed, Checksum, Feed, PrereleasePolicy, Product, Release,
};
use base64::Engine as _;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, IF_NONE_MATCH};
use semver::Version;
use serde::Deserialize;
use std::collections::BTreeMap;

/// Reads package versions from an npm registry.
#[derive(Clone, Debug)]
pub struct NpmFeed {
    pub client: reqwest::Client,
    /// Registry root, `https://registry.npmjs.org` in production.
    pub registry_base: String,
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

impl NpmFeed {
    pub fn new(client: reqwest::Client, registry_base: impl Into<String>) -> Self {
        Self { client, registry_base: registry_base.into() }
    }

    /// Lists the versions of `product`'s package. Sends the cached `ETag` as
    /// `If-None-Match`. Deprecated versions are left out, and under
    /// [`PrereleasePolicy::Stable`] so is every version above the `latest`
    /// dist-tag, which is what the publisher released as stable.
    pub async fn fetch(&self, product: &Product, cached: Option<&CachedFeed>) -> Result<FeedResponse> {
        let Feed::Npm { package } = &product.feed else {
            return Err(FeedError::WrongFeed { product: product.id.clone() });
        };
        let url = format!(
            "{}/{}",
            self.registry_base.trim_end_matches('/'),
            package.replace('/', "%2F")
        );
        let mut request = self.client.get(&url).header(ACCEPT, "application/json");
        if let Some(tag) = cached.and_then(|c| c.etag.as_deref()) {
            request = request.header(IF_NONE_MATCH, tag);
        }
        let response =
            request.send().await.map_err(|source| FeedError::Http { url: url.clone(), source })?;
        if response.status() == StatusCode::NOT_MODIFIED {
            return Ok(FeedResponse::NotModified);
        }
        if !response.status().is_success() {
            return Err(reject(&url, &response));
        }
        let rate_limit = super::rate_limit(&response);
        let new_etag = etag(&response);
        let packument: Packument = read_json(&url, response).await?;

        let latest = match packument.dist_tags.get("latest") {
            Some(tag) => Some(Version::parse(tag).map_err(|source| FeedError::BadVersion {
                version: tag.clone(),
                source,
            })?),
            None => None,
        };
        let cap = match product.prereleases {
            PrereleasePolicy::Stable => latest.as_ref(),
            PrereleasePolicy::Include => None,
        };

        let mut releases = Vec::new();
        for (key, entry) in packument.versions {
            if entry.deprecated.is_some() {
                continue;
            }
            let version = match Version::parse(&key) {
                Ok(version) => version,
                Err(e) => {
                    tracing::debug!(version = %key, error = %e, "skipping non-semver npm version");
                    continue;
                }
            };
            if cap.is_some_and(|cap| &version > cap) {
                continue;
            }
            let checksum = entry.dist.integrity.as_deref().and_then(sha512_hex).map(Checksum::Sha512);
            let name = entry
                .dist
                .tarball
                .rsplit('/')
                .next()
                .filter(|n| !n.is_empty())
                .unwrap_or("package.tgz")
                .to_owned();
            let published_at =
                packument.time.get(&key).and_then(|t| t.as_str()).map(str::to_owned);
            releases.push(Release {
                product: product.id.clone(),
                version,
                notes: String::new(),
                published_at,
                artifact: Some(Artifact {
                    name,
                    url: entry.dist.tarball,
                    size: None,
                    checksum,
                    format: ArchiveFormat::NpmTarball,
                    signature_url: None,
                }),
            });
        }
        Ok(FeedResponse::Fresh { feed: CachedFeed { etag: new_etag, releases }, rate_limit })
    }
}

/// The SHA-512 of an SRI `integrity` string (`sha512-<base64>`, possibly
/// alongside other hashes), as hex.
fn sha512_hex(integrity: &str) -> Option<String> {
    integrity.split_whitespace().find_map(|part| {
        let b64 = part.strip_prefix("sha512-")?;
        let b64 = b64.split('?').next().unwrap_or(b64);
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
        (bytes.len() == 64).then(|| hex::encode(bytes))
    })
}
