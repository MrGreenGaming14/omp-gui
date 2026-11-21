//! Installs a release beside the active version under `<root>/versions/<product>/<version>/`
//! (`payload/`, `artifact/`, `staged.json`). Work happens in a temporary directory renamed into
//! place with `staged.json` written last, so a failed staging leaves nothing behind.

use loa_settings::updates::{Artifact, Checksum, Release};
use crate::{Error, Result, io_at, unpack};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::fs;
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub const MANIFEST_FILE: &str = "staged.json";
/// Largest detached signature accepted.
const SIGNATURE_LIMIT: u64 = 64 * 1024;
/// Ceiling even when a registry reports no artifact size.
const MAX_ARTIFACT_SIZE: u64 = 1024 * 1024 * 1024;

/// Directory holding every version of `product`.
pub fn product_dir(root: &Path, product: &str) -> PathBuf {
    root.join("versions").join(product)
}

pub fn version_dir(root: &Path, product: &str, version: &Version) -> PathBuf {
    product_dir(root, product).join(version.to_string())
}

/// Written to `staged.json` once a version is fully staged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StagedManifest {
    pub product: String,
    pub version: Version,
    /// File name inside `artifact/`.
    pub artifact_name: String,
    pub sha256: String,
    pub size: u64,
    pub signature_name: Option<String>,
}

/// A fully staged version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Staged {
    pub dir: PathBuf,
    pub manifest: StagedManifest,
}

impl Staged {
    pub fn payload(&self) -> PathBuf {
        self.dir.join("payload")
    }

    pub fn artifact(&self) -> PathBuf {
        self.dir.join("artifact").join(&self.manifest.artifact_name)
    }

    pub fn signature(&self) -> Option<PathBuf> {
        self.manifest.signature_name.as_ref().map(|name| self.dir.join("artifact").join(name))
    }
}

/// The staged version of `product` at `version`, when fully staged.
pub fn staged(root: &Path, product: &str, version: &Version) -> Result<Option<Staged>> {
    read_staged(&version_dir(root, product, version))
}

fn read_staged(dir: &Path) -> Result<Option<Staged>> {
    Ok(crate::read_json(&dir.join(MANIFEST_FILE))?.map(|manifest| Staged { dir: dir.to_owned(), manifest }))
}

/// Downloads, verifies and unpacks releases under an updater root.
#[derive(Clone, Debug)]
pub struct Stager {
    client: reqwest::Client,
    root: PathBuf,
}

impl Stager {
    pub fn new(client: reqwest::Client, root: impl Into<PathBuf>) -> Self {
        Self { client, root: root.into() }
    }

    /// Stages `release`; returns the existing staging when that version is already staged.
    pub async fn stage(&self, release: &Release) -> Result<Staged> {
        let unverifiable = |what| Error::Unverifiable { product: release.product.clone(), version: release.version.clone(), what };
        let artifact = release.artifact.as_ref().ok_or_else(|| unverifiable("artifact for this platform"))?;
        let checksum = artifact.checksum.as_ref().ok_or_else(|| unverifiable("checksum"))?;
        single_name(&release.product)?;
        single_name(&artifact.name)?;
        if artifact.size.is_some_and(|size| size > MAX_ARTIFACT_SIZE) {
            return Err(Error::TooLarge { url: artifact.url.clone(), limit: MAX_ARTIFACT_SIZE });
        }
        let final_dir = version_dir(&self.root, &release.product, &release.version);
        if let Some(staged) = read_staged(&final_dir)? {
            return Ok(staged);
        }
        if final_dir.exists() {
            let entry = final_dir.display().to_string();
            return Err(Error::UnsafeEntry { entry, reason: "version directory exists without a staged manifest" });
        }
        let product_dir = product_dir(&self.root, &release.product);
        fs::create_dir_all(&product_dir).map_err(io_at(&product_dir))?;
        let tmp = tempfile::Builder::new().prefix(".staging-").tempdir_in(&product_dir).map_err(io_at(&product_dir))?;
        let artifact_dir = tmp.path().join("artifact");
        fs::create_dir(&artifact_dir).map_err(io_at(&artifact_dir))?;
        let artifact_path = artifact_dir.join(&artifact.name);
        let sha512 = matches!(checksum, Checksum::Sha512(_));
        let got = self.download(&artifact.url, &artifact_path, artifact.size.unwrap_or(MAX_ARTIFACT_SIZE), sha512).await?;
        verify(artifact, checksum, &got)?;
        let signature_name = match &artifact.signature_url {
            Some(url) => {
                let name = format!("{}.sig", artifact.name);
                self.download(url, &artifact_dir.join(&name), SIGNATURE_LIMIT, false).await?;
                Some(name)
            }
            None => None,
        };
        let (format, payload) = (artifact.format, tmp.path().join("payload"));
        let unpacking = artifact_path.clone();
        tokio::task::spawn_blocking(move || unpack::unpacker(format).unpack(&unpacking, &payload))
            .await
            .map_err(|e| Error::Archive(format!("unpacking task failed: {e}")))??;
        let manifest = StagedManifest {
            product: release.product.clone(),
            version: release.version.clone(),
            artifact_name: artifact.name.clone(),
            sha256: got.sha256,
            size: got.size,
            signature_name,
        };
        crate::write_json(&tmp.path().join(MANIFEST_FILE), &manifest)?;
        if let Err(source) = fs::rename(tmp.path(), &final_dir) {
            // Another stager may have finished the same version first.
            return read_staged(&final_dir)?.ok_or(Error::Io { path: final_dir, source });
        }
        // The directory now lives at `final_dir`; the guard must not delete it.
        let _ = tmp.keep();
        loa_util::fs::sync_dir(&product_dir).map_err(io_at(&product_dir))?;
        Ok(Staged { dir: final_dir, manifest })
    }

    /// Streams `url` into `dest`, hashing as it writes; fails once more than `limit` bytes arrive.
    async fn download(&self, url: &str, dest: &Path, limit: u64, sha512: bool) -> Result<Downloaded> {
        let http = |e: reqwest::Error| Error::Http { url: url.to_owned(), reason: e.to_string() };
        let too_large = || Error::TooLarge { url: url.to_owned(), limit };
        let mut response = self.client.get(url).send().await.map_err(http)?;
        if !response.status().is_success() {
            return Err(Error::Status { url: url.to_owned(), status: response.status().as_u16() });
        }
        if response.content_length().is_some_and(|length| length > limit) {
            return Err(too_large());
        }
        let mut file = tokio::fs::File::create(dest).await.map_err(io_at(dest))?;
        let (mut sha256, mut sha512, mut size) = (Sha256::new(), sha512.then(Sha512::new), 0u64);
        while let Some(chunk) = response.chunk().await.map_err(http)? {
            size = size.saturating_add(chunk.len() as u64);
            if size > limit {
                return Err(too_large());
            }
            sha256.update(&chunk);
            if let Some(h) = sha512.as_mut() {
                h.update(&chunk);
            }
            file.write_all(&chunk).await.map_err(io_at(dest))?;
        }
        file.flush().await.map_err(io_at(dest))?;
        file.sync_all().await.map_err(io_at(dest))?;
        Ok(Downloaded { size, sha256: hex::encode(sha256.finalize()), sha512: sha512.map(|h| hex::encode(h.finalize())) })
    }
}

struct Downloaded {
    size: u64,
    sha256: String,
    sha512: Option<String>,
}

/// Checks a download against the feed's size and checksum.
fn verify(artifact: &Artifact, checksum: &Checksum, got: &Downloaded) -> Result<()> {
    if let Some(expected) = artifact.size.filter(|expected| *expected != got.size) {
        return Err(Error::Mismatch { what: "size", expected: expected.to_string(), actual: got.size.to_string() });
    }
    let (what, expected, actual) = match checksum {
        Checksum::Sha256(expected) => ("sha256", expected, Some(&got.sha256)),
        Checksum::Sha512(expected) => ("sha512", expected, got.sha512.as_ref()),
    };
    let actual = actual.cloned().unwrap_or_default();
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err(Error::Mismatch { what, expected: expected.clone(), actual });
    }
    Ok(())
}

/// Fails unless `name` is one normal path component.
fn single_name(name: &str) -> Result<()> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(Error::BadName(name.to_owned())),
    }
}
