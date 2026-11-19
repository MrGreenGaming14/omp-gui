//! GitHub releases feed.

use super::{FeedError, FeedResponse, Result, etag, read_json, reject};
use crate::product::{
    ArchiveFormat, Artifact, CachedFeed, Checksum, Feed, PrereleasePolicy, Product, Release,
};
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, IF_NONE_MATCH};
use semver::Version;
use serde::Deserialize;

/// Reads releases of GitHub repositories through the REST API.
#[derive(Clone, Debug)]
pub struct GitHubFeed {
    pub client: reqwest::Client,
    /// API root, `https://api.github.com` in production.
    pub api_base: String,
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

impl GitHubFeed {
    pub fn new(client: reqwest::Client, api_base: impl Into<String>) -> Self {
        Self { client, api_base: api_base.into() }
    }

    /// Lists the releases of `product`'s repository. Sends the cached
    /// `ETag` as `If-None-Match`, so an unchanged feed answers 304, which
    /// GitHub does not count against the rate limit. Drafts, tags that are
    /// not semver and, under [`PrereleasePolicy::Stable`], releases flagged
    /// as prereleases are left out.
    pub async fn fetch(
        &self,
        product: &Product,
        cached: Option<&CachedFeed>,
        token: Option<&str>,
    ) -> Result<FeedResponse> {
        let Feed::GitHub { owner, repo } = &product.feed else {
            return Err(FeedError::WrongFeed { product: product.id.clone() });
        };
        let url = format!(
            "{}/repos/{owner}/{repo}/releases?per_page=100",
            self.api_base.trim_end_matches('/')
        );
        let mut request = self
            .client
            .get(&url)
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
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
        let listed: Vec<GhRelease> = read_json(&url, response).await?;

        let mut releases = Vec::new();
        for release in listed {
            if release.draft
                || (release.prerelease && product.prereleases == PrereleasePolicy::Stable)
            {
                continue;
            }
            let tag = release.tag_name.strip_prefix('v').unwrap_or(&release.tag_name);
            let version = match Version::parse(tag) {
                Ok(version) => version,
                Err(e) => {
                    tracing::debug!(tag = %release.tag_name, error = %e, "skipping non-semver release tag");
                    continue;
                }
            };
            let artifact = match &product.asset {
                Some(pattern) => self.artifact(pattern, &version, &release.assets).await?,
                None => None,
            };
            releases.push(Release {
                product: product.id.clone(),
                version,
                notes: release.body.unwrap_or_default(),
                published_at: release.published_at,
                artifact,
            });
        }
        Ok(FeedResponse::Fresh { feed: CachedFeed { etag: new_etag, releases }, rate_limit })
    }

    /// The asset named by `pattern` for `version`, with its checksum and
    /// signature URL.
    async fn artifact(
        &self,
        pattern: &str,
        version: &Version,
        assets: &[GhAsset],
    ) -> Result<Option<Artifact>> {
        let name = pattern.replace("{version}", &version.to_string());
        let Some(asset) = assets.iter().find(|a| a.name == name) else {
            return Ok(None);
        };
        let checksum = match asset.digest.as_deref().and_then(|d| d.strip_prefix("sha256:")) {
            Some(hex) if is_sha256_hex(hex) => Some(Checksum::Sha256(hex.to_ascii_lowercase())),
            _ => {
                let sidecar = format!("{name}.sha256");
                match assets.iter().find(|a| a.name == sidecar) {
                    Some(sidecar) => self.sidecar_sha256(&sidecar.browser_download_url).await?,
                    None => None,
                }
            }
        };
        let signature = format!("{name}.sig");
        let signature_url =
            assets.iter().find(|a| a.name == signature).map(|a| a.browser_download_url.clone());
        Ok(Some(Artifact {
            format: format_of(&name),
            url: asset.browser_download_url.clone(),
            size: asset.size,
            checksum,
            signature_url,
            name,
        }))
    }

    /// Reads a `.sha256` sidecar (`<hex>` or `<hex>  <file>`). A sidecar
    /// that holds no SHA-256 leaves the artifact without a checksum, which
    /// staging refuses.
    async fn sidecar_sha256(&self, url: &str) -> Result<Option<Checksum>> {
        let response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|source| FeedError::Http { url: url.to_owned(), source })?;
        if !response.status().is_success() {
            return Err(reject(url, &response));
        }
        let text =
            response.text().await.map_err(|source| FeedError::Http { url: url.to_owned(), source })?;
        match text.split_whitespace().next() {
            Some(hex) if is_sha256_hex(hex) => Ok(Some(Checksum::Sha256(hex.to_ascii_lowercase()))),
            _ => {
                tracing::warn!(%url, "sha256 sidecar holds no SHA-256 digest");
                Ok(None)
            }
        }
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Archive format from an asset's file name.
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
