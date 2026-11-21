//! Installs a release side by side with the active version.
//!
//! A release is downloaded into a temporary directory inside the product's
//! directory, verified against the feed's size and checksum, unpacked into
//! `payload/`, and only then renamed to `<root>/versions/<product>/<version>`
//! with `staged.json` already in place. Other versions are never touched,
//! and a failed staging leaves nothing behind.

use loa_util::fs::{sync_dir, write_atomic};
use crate::layout::{self, ARTIFACT_DIR, MANIFEST_FILE, PAYLOAD_DIR, Staged, StagedManifest};
use crate::product::{ArchiveFormat, Artifact, Checksum, Release};
use semver::Version;
use sha2::{Digest, Sha256, Sha512};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncWriteExt;

/// Errors staging a release.
#[derive(Debug, thiserror::Error)]
pub enum StageError {
    #[error("{product} {version} has no artifact for this platform")]
    NoArtifact { product: String, version: Version },
    #[error("{product} {version} has no checksum; refusing to install unverified bytes")]
    NoChecksum { product: String, version: Version },
    #[error("{0:?} is not a single file name")]
    BadName(String),
    #[error("request to {url} failed: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },
    #[error("{url} answered HTTP {status}")]
    Status { url: String, status: u16 },
    #[error("{url} sent more than {limit} bytes")]
    TooLarge { url: String, limit: u64 },
    #[error("artifact size mismatch: expected {expected} bytes, got {actual}")]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("artifact {algorithm} mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch { algorithm: &'static str, expected: String, actual: String },
    #[error("archive entry {entry:?} rejected: {reason}")]
    UnsafeEntry { entry: String, reason: &'static str },
    #[error("archive entry {entry:?} has an unsupported type")]
    UnsupportedEntry { entry: String },
    #[error("{}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("reading zip archive: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("{}: {source}", path.display())]
    Manifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("unpacking task failed: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub type Result<T, E = StageError> = std::result::Result<T, E>;

/// Largest detached signature accepted.
const SIGNATURE_LIMIT: u64 = 64 * 1024;
/// Hard ceiling even when a registry does not report an artifact size.
const MAX_ARTIFACT_SIZE: u64 = 1024 * 1024 * 1024;


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

    /// Stages `release` into `<root>/versions/<product>/<version>`. Returns
    /// the existing staging when that version is already staged.
    pub async fn stage(&self, release: &Release) -> Result<Staged> {
        let artifact = release.artifact.as_ref().ok_or_else(|| StageError::NoArtifact {
            product: release.product.clone(),
            version: release.version.clone(),
        })?;
        let checksum = artifact.checksum.as_ref().ok_or_else(|| StageError::NoChecksum {
            product: release.product.clone(),
            version: release.version.clone(),
        })?;
        single_name(&release.product)?;
        if artifact.size.is_some_and(|size| size > MAX_ARTIFACT_SIZE) {
            return Err(StageError::TooLarge { url: artifact.url.clone(), limit: MAX_ARTIFACT_SIZE });
        }
        single_name(&artifact.name)?;

        let final_dir = layout::version_dir(&self.root, &release.product, &release.version);
        if final_dir.join(MANIFEST_FILE).exists() {
            return read_staged(&final_dir);
        }
        if final_dir.exists() {
            return Err(StageError::UnsafeEntry {
                entry: final_dir.display().to_string(),
                reason: "version directory exists without a staged manifest",
            });
        }

        let product_dir = layout::product_dir(&self.root, &release.product);
        fs::create_dir_all(&product_dir).map_err(io_at(&product_dir))?;
        let tmp = tempfile::Builder::new()
            .prefix(".staging-")
            .tempdir_in(&product_dir)
            .map_err(io_at(&product_dir))?;
        let work = tmp.path().to_path_buf();

        let artifact_dir = work.join(ARTIFACT_DIR);
        fs::create_dir(&artifact_dir).map_err(io_at(&artifact_dir))?;
        let artifact_path = artifact_dir.join(&artifact.name);
        let downloaded = self
            .download(
                &artifact.url,
                &artifact_path,
                Some(artifact.size.unwrap_or(MAX_ARTIFACT_SIZE)),
                matches!(checksum, Checksum::Sha512(_)),
            )
            .await?;
        verify(artifact, checksum, &downloaded)?;

        let signature_name = match &artifact.signature_url {
            Some(url) => {
                let name = format!("{}.sig", artifact.name);
                self.download(url, &artifact_dir.join(&name), Some(SIGNATURE_LIMIT), false).await?;
                Some(name)
            }
            None => None,
        };

        let payload = work.join(PAYLOAD_DIR);
        let format = artifact.format;
        let name = artifact.name.clone();
        let unpack_from = artifact_path.clone();
        let unpack_into = payload.clone();
        tokio::task::spawn_blocking(move || unpack(format, &unpack_from, &name, &unpack_into))
            .await??;

        let manifest = StagedManifest {
            product: release.product.clone(),
            version: release.version.clone(),
            artifact_name: artifact.name.clone(),
            sha256: downloaded.sha256,
            size: downloaded.size,
            signature_name,
        };
        let manifest_path = work.join(MANIFEST_FILE);
        let json = serde_json::to_vec_pretty(&manifest)
            .map_err(|source| StageError::Manifest { path: manifest_path.clone(), source })?;
        write_atomic(&manifest_path, &json, None).map_err(io_at(&manifest_path))?;

        if let Err(e) = fs::rename(&work, &final_dir) {
            // Another stager may have finished the same version first.
            if final_dir.join(MANIFEST_FILE).exists() {
                return read_staged(&final_dir);
            }
            return Err(StageError::Io { path: final_dir, source: e });
        }
        // The directory now lives at `final_dir`; keep the guard from
        // deleting the (no longer existing) temporary path.
        let _ = tmp.keep();
        sync_dir(&product_dir).map_err(io_at(&product_dir))?;
        Ok(Staged { dir: final_dir, manifest })
    }

    /// Streams `url` into `dest`, hashing while writing. Fails as soon as
    /// more than `limit` bytes arrive.
    async fn download(
        &self,
        url: &str,
        dest: &Path,
        limit: Option<u64>,
        want_sha512: bool,
    ) -> Result<Downloaded> {
        let http = |source| StageError::Http { url: url.to_owned(), source };
        let mut response = self.client.get(url).send().await.map_err(http)?;
        if !response.status().is_success() {
            return Err(StageError::Status { url: url.to_owned(), status: response.status().as_u16() });
        }
        let too_large = |limit| StageError::TooLarge { url: url.to_owned(), limit };
        if let (Some(limit), Some(length)) = (limit, response.content_length())
            && length > limit
        {
            return Err(too_large(limit));
        }

        let mut file = tokio::fs::File::create(dest).await.map_err(io_at(dest))?;
        let mut sha256 = Sha256::new();
        let mut sha512 = want_sha512.then(Sha512::new);
        let mut size: u64 = 0;
        while let Some(chunk) = response.chunk().await.map_err(http)? {
            size = size.saturating_add(chunk.len() as u64);
            if let Some(limit) = limit
                && size > limit
            {
                return Err(too_large(limit));
            }
            sha256.update(&chunk);
            if let Some(h) = sha512.as_mut() {
                h.update(&chunk);
            }
            file.write_all(&chunk).await.map_err(io_at(dest))?;
        }
        file.flush().await.map_err(io_at(dest))?;
        file.sync_all().await.map_err(io_at(dest))?;
        Ok(Downloaded {
            size,
            sha256: hex::encode(sha256.finalize()),
            sha512: sha512.map(|h| hex::encode(h.finalize())),
        })
    }
}

/// Size and digests of a downloaded file.
struct Downloaded {
    size: u64,
    sha256: String,
    sha512: Option<String>,
}

/// Checks a download against the feed's size and checksum.
fn verify(artifact: &Artifact, checksum: &Checksum, got: &Downloaded) -> Result<()> {
    if let Some(expected) = artifact.size
        && expected != got.size
    {
        return Err(StageError::SizeMismatch { expected, actual: got.size });
    }
    let (algorithm, expected, actual) = match checksum {
        Checksum::Sha256(expected) => ("sha256", expected, Some(&got.sha256)),
        Checksum::Sha512(expected) => ("sha512", expected, got.sha512.as_ref()),
    };
    let actual = actual.cloned().unwrap_or_default();
    if !expected.eq_ignore_ascii_case(&actual) {
        return Err(StageError::ChecksumMismatch {
            algorithm,
            expected: expected.clone(),
            actual,
        });
    }
    Ok(())
}

/// The staged version of `product` at `version`, if it is fully staged.
pub fn staged(root: &Path, product: &str, version: &Version) -> Result<Option<Staged>> {
    let dir = layout::version_dir(root, product, version);
    if !dir.join(MANIFEST_FILE).exists() {
        return Ok(None);
    }
    read_staged(&dir).map(Some)
}

/// Reads the staged version in `dir` from its `staged.json`.
pub fn read_staged(dir: &Path) -> Result<Staged> {
    let path = dir.join(MANIFEST_FILE);
    let bytes = fs::read(&path).map_err(io_at(&path))?;
    let manifest = serde_json::from_slice(&bytes)
        .map_err(|source| StageError::Manifest { path: path.clone(), source })?;
    Ok(Staged { dir: dir.to_path_buf(), manifest })
}

fn io_at(path: &Path) -> impl FnOnce(io::Error) -> StageError + '_ {
    move |source| StageError::Io { path: path.to_path_buf(), source }
}

/// Fails unless `name` is one normal path component.
fn single_name(name: &str) -> Result<()> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Ok(()),
        _ => Err(StageError::BadName(name.to_owned())),
    }
}

/// Unpacks the downloaded `artifact` into `payload`.
fn unpack(format: ArchiveFormat, artifact: &Path, name: &str, payload: &Path) -> Result<()> {
    fs::create_dir(payload).map_err(io_at(payload))?;
    let open = || fs::File::open(artifact).map_err(io_at(artifact));
    match format {
        ArchiveFormat::TarGz => unpack_tar(open()?, payload, 0),
        ArchiveFormat::NpmTarball => unpack_tar(open()?, payload, 1),
        ArchiveFormat::Zip => unpack_zip(open()?, payload),
        ArchiveFormat::Raw => {
            let dest = payload.join(name);
            let mut source = open()?;
            let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(io_at(&dest))?;
            io::copy(&mut source, &mut file).map_err(io_at(&dest))?;
            close_synced(file, &dest)?;
            set_mode(&dest, 0o755)?;
            Ok(())
        }
    }
}

/// Unpacks a gzipped tar, dropping the first `strip` path components of
/// every entry (npm tarballs put everything under `package/`).
fn unpack_tar(file: fs::File, payload: &Path, strip: usize) -> Result<()> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(file)));
    let tar_err = |source| StageError::Io { path: payload.to_path_buf(), source };
    for entry in archive.entries().map_err(tar_err)? {
        let mut entry = entry.map_err(tar_err)?;
        let raw = entry.path().map_err(tar_err)?.into_owned();
        let shown = raw.display().to_string();
        let Some(rel) = entry_path(&raw, strip, &shown)? else {
            continue;
        };
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            make_dirs(payload, &rel, &shown)?;
        } else if kind.is_file() || kind.is_contiguous() {
            let mode = entry.header().mode().map_err(tar_err)?;
            write_file(payload, &rel, &shown, &mut entry, mode)?;
        } else if kind.is_symlink() {
            let target = entry
                .link_name()
                .map_err(tar_err)?
                .ok_or(StageError::UnsafeEntry { entry: shown.clone(), reason: "symlink without target" })?
                .into_owned();
            make_symlink(payload, &rel, &target, &shown)?;
        } else if kind.is_hard_link() {
            let target = entry
                .link_name()
                .map_err(tar_err)?
                .ok_or(StageError::UnsafeEntry { entry: shown.clone(), reason: "hard link without target" })?
                .into_owned();
            let Some(target_rel) = entry_path(&target, strip, &shown)? else {
                return Err(StageError::UnsafeEntry { entry: shown, reason: "hard link to the archive root" });
            };
            make_hard_link(payload, &rel, &target_rel, &shown)?;
        } else if kind.is_pax_global_extensions() {
            continue;
        } else {
            return Err(StageError::UnsupportedEntry { entry: shown });
        }
    }
    Ok(())
}

fn unpack_zip(file: fs::File, payload: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(io::BufReader::new(file))?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let shown = entry.name()?.into_owned();
        let Some(rel) = entry_path(Path::new(&shown), 0, &shown)? else {
            continue;
        };
        if entry.is_dir() {
            make_dirs(payload, &rel, &shown)?;
        } else if entry.is_symlink() {
            let mut target = String::new();
            entry.read_to_string(&mut target).map_err(io_at(payload))?;
            make_symlink(payload, &rel, Path::new(&target), &shown)?;
        } else {
            let mode = entry.unix_mode().unwrap_or(0o644);
            write_file(payload, &rel, &shown, &mut entry, mode)?;
        }
    }
    Ok(())
}

/// The payload-relative path of an archive entry with `strip` leading
/// components removed; `None` when nothing remains. Absolute paths and
/// `..` components are rejected.
fn entry_path(raw: &Path, strip: usize, shown: &str) -> Result<Option<PathBuf>> {
    let mut out = PathBuf::new();
    let mut skipped = 0;
    for component in raw.components() {
        match component {
            Component::Normal(part) if skipped < strip => {
                let _ = part;
                skipped += 1;
            }
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(StageError::UnsafeEntry { entry: shown.to_owned(), reason: "path contains `..`" });
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(StageError::UnsafeEntry { entry: shown.to_owned(), reason: "absolute path" });
            }
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// Creates `payload/rel` and its parents as real directories. Fails when
/// any of them already exists as a symlink, so no entry is ever written
/// through a link.
fn make_dirs(payload: &Path, rel: &Path, shown: &str) -> Result<()> {
    let mut path = payload.to_path_buf();
    for component in rel.components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(meta) if meta.file_type().is_symlink() => {
                return Err(StageError::UnsafeEntry {
                    entry: shown.to_owned(),
                    reason: "path runs through a symlink",
                });
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(StageError::UnsafeEntry {
                    entry: shown.to_owned(),
                    reason: "path runs through a file",
                });
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                fs::create_dir(&path).map_err(io_at(&path))?;
            }
            Err(e) => return Err(StageError::Io { path, source: e }),
        }
    }
    Ok(())
}

/// Prepares `payload/rel` for a new non-directory entry: creates its
/// parents and removes a file or link an earlier entry left at that path.
fn prepare_leaf(payload: &Path, rel: &Path, shown: &str) -> Result<PathBuf> {
    if let Some(parent) = rel.parent() {
        make_dirs(payload, parent, shown)?;
    }
    let dest = payload.join(rel);
    match fs::symlink_metadata(&dest) {
        Ok(meta) if meta.is_dir() => Err(StageError::UnsafeEntry {
            entry: shown.to_owned(),
            reason: "replaces a directory",
        }),
        Ok(_) => {
            fs::remove_file(&dest).map_err(io_at(&dest))?;
            Ok(dest)
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(dest),
        Err(e) => Err(StageError::Io { path: dest, source: e }),
    }
}

fn write_file(payload: &Path, rel: &Path, shown: &str, data: &mut dyn Read, mode: u32) -> Result<()> {
    let dest = prepare_leaf(payload, rel, shown)?;
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(io_at(&dest))?;
    io::copy(data, &mut file).map_err(io_at(&dest))?;
    close_synced(file, &dest)?;
    // Keep the owner able to read and replace what was unpacked.
    set_mode(&dest, (mode & 0o777) | 0o600)
}

/// Flushes `file` to disk and closes it, so this process holds no writable
/// descriptor once the file may be run. A child forked meanwhile can still
/// hold one briefly; launches go through `loa_util::exec::retry_busy` for that.
fn close_synced(file: fs::File, path: &Path) -> Result<()> {
    file.sync_all().map_err(io_at(path))
}

/// Creates a symlink whose target stays inside `payload`. The target may
/// only climb (`..`) before it descends, so the climb is measured from the
/// link's own real directory and cannot be redirected by other links.
fn make_symlink(payload: &Path, rel: &Path, target: &Path, shown: &str) -> Result<()> {
    let unsafe_target = |reason| StageError::UnsafeEntry { entry: shown.to_owned(), reason };
    let mut depth = rel.parent().map_or(0, |p| p.components().count());
    let mut descended = false;
    for component in target.components() {
        match component {
            Component::ParentDir if descended => return Err(unsafe_target("symlink target climbs after descending")),
            Component::ParentDir => {
                depth = depth.checked_sub(1).ok_or_else(|| unsafe_target("symlink points outside the payload"))?;
            }
            Component::Normal(_) => descended = true,
            Component::CurDir => {}
            Component::RootDir | Component::Prefix(_) => return Err(unsafe_target("symlink target is absolute")),
        }
    }
    if target.as_os_str().is_empty() {
        return Err(unsafe_target("symlink without target"));
    }
    let dest = prepare_leaf(payload, rel, shown)?;
    symlink(target, &dest, shown)
}

#[cfg(unix)]
fn symlink(target: &Path, dest: &Path, _shown: &str) -> Result<()> {
    std::os::unix::fs::symlink(target, dest).map_err(io_at(dest))
}

#[cfg(not(unix))]
fn symlink(_target: &Path, _dest: &Path, shown: &str) -> Result<()> {
    Err(StageError::UnsupportedEntry { entry: shown.to_owned() })
}

/// Hard links `payload/rel` to an already unpacked regular file.
fn make_hard_link(payload: &Path, rel: &Path, target: &Path, shown: &str) -> Result<()> {
    if let Some(parent) = target.parent() {
        // Rejects targets reached through a symlink.
        make_dirs(payload, parent, shown)?;
    }
    let source = payload.join(target);
    match fs::symlink_metadata(&source) {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            return Err(StageError::UnsafeEntry {
                entry: shown.to_owned(),
                reason: "hard link target is not a regular file",
            });
        }
        Err(e) => return Err(StageError::Io { path: source, source: e }),
    }
    let dest = prepare_leaf(payload, rel, shown)?;
    fs::hard_link(&source, &dest).map_err(io_at(&dest))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_at(path))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> Result<()> {
    Ok(())
}
