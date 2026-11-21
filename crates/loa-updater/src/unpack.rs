//! [`Unpacker`] strategies writing into one [`EntrySink`] that keeps every entry inside the payload.

use crate::model::ArchiveFormat;
use crate::{Error, Result, io_at};
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};

/// Unpacks an artifact into a payload directory it creates.
pub trait Unpacker: Send {
    fn unpack(&self, artifact: &Path, payload: &Path) -> Result<()>;
}

/// A gzipped tar, dropping the first `strip` components (npm tarballs nest under `package/`).
pub struct TarGz {
    pub strip: usize,
}

pub struct Zip;

/// A single executable file kept under its own name.
pub struct Raw;

/// The strategy for `format`.
pub fn unpacker(format: ArchiveFormat) -> Box<dyn Unpacker> {
    match format {
        ArchiveFormat::TarGz => Box::new(TarGz { strip: 0 }),
        ArchiveFormat::NpmTarball => Box::new(TarGz { strip: 1 }),
        ArchiveFormat::Zip => Box::new(Zip),
        ArchiveFormat::Raw => Box::new(Raw),
    }
}

impl Unpacker for TarGz {
    fn unpack(&self, artifact: &Path, payload: &Path) -> Result<()> {
        let sink = EntrySink::create(payload)?;
        let file = fs::File::open(artifact).map_err(io_at(artifact))?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(io::BufReader::new(file)));
        let tar = io_at(payload);
        for entry in archive.entries().map_err(tar)? {
            let mut entry = entry.map_err(io_at(payload))?;
            let raw = entry.path().map_err(io_at(payload))?.into_owned();
            let shown = raw.display().to_string();
            let Some(rel) = relative(&raw, self.strip, &shown)? else { continue };
            let kind = entry.header().entry_type();
            let link = || -> Result<PathBuf> {
                let target = entry.link_name().map_err(io_at(payload))?;
                target.map(|t| t.into_owned()).ok_or_else(|| unsafe_entry(&shown, "link without target"))
            };
            if kind.is_dir() {
                sink.dirs(&rel, &shown)?;
            } else if kind.is_symlink() {
                sink.symlink(&rel, &link()?, &shown)?;
            } else if kind.is_hard_link() {
                let target = relative(&link()?, self.strip, &shown)?.ok_or_else(|| unsafe_entry(&shown, "hard link to the archive root"))?;
                sink.hard_link(&rel, &target, &shown)?;
            } else if kind.is_file() || kind.is_contiguous() {
                let mode = entry.header().mode().map_err(io_at(payload))?;
                sink.file(&rel, &shown, &mut entry, mode)?;
            } else if !kind.is_pax_global_extensions() {
                return Err(unsafe_entry(&shown, "unsupported entry type"));
            }
        }
        Ok(())
    }
}

impl Unpacker for Zip {
    fn unpack(&self, artifact: &Path, payload: &Path) -> Result<()> {
        let sink = EntrySink::create(payload)?;
        let file = fs::File::open(artifact).map_err(io_at(artifact))?;
        let zip_err = |e: zip::result::ZipError| Error::Archive(e.to_string());
        let mut archive = zip::ZipArchive::new(io::BufReader::new(file)).map_err(zip_err)?;
        for index in 0..archive.len() {
            let mut entry = archive.by_index(index).map_err(zip_err)?;
            let shown = entry.name().map_err(zip_err)?.into_owned();
            let Some(rel) = relative(Path::new(&shown), 0, &shown)? else { continue };
            if entry.is_dir() {
                sink.dirs(&rel, &shown)?;
            } else if entry.is_symlink() {
                let mut target = String::new();
                entry.read_to_string(&mut target).map_err(io_at(payload))?;
                sink.symlink(&rel, Path::new(&target), &shown)?;
            } else {
                let mode = entry.unix_mode().unwrap_or(0o644);
                sink.file(&rel, &shown, &mut entry, mode)?;
            }
        }
        Ok(())
    }
}

impl Unpacker for Raw {
    fn unpack(&self, artifact: &Path, payload: &Path) -> Result<()> {
        let sink = EntrySink::create(payload)?;
        let name = artifact.file_name().ok_or_else(|| Error::BadName(artifact.display().to_string()))?;
        let mut source = fs::File::open(artifact).map_err(io_at(artifact))?;
        sink.file(Path::new(name), &name.to_string_lossy(), &mut source, 0o755)
    }
}

fn unsafe_entry(entry: &str, reason: &'static str) -> Error {
    Error::UnsafeEntry { entry: entry.to_owned(), reason }
}

/// The payload-relative path of an entry minus `strip` leading parts; `None` when nothing remains.
fn relative(raw: &Path, strip: usize, shown: &str) -> Result<Option<PathBuf>> {
    let mut out = PathBuf::new();
    for (index, part) in raw.components().filter(|c| *c != Component::CurDir).enumerate() {
        match part {
            Component::Normal(_) if index < strip => {}
            Component::Normal(name) => out.push(name),
            Component::ParentDir => return Err(unsafe_entry(shown, "path contains `..`")),
            _ => return Err(unsafe_entry(shown, "absolute path")),
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// Writes entries under `root`; every path is confined to it, links included.
struct EntrySink {
    root: PathBuf,
}

impl EntrySink {
    fn create(root: &Path) -> Result<Self> {
        fs::create_dir(root).map_err(io_at(root))?;
        Ok(Self { root: root.to_owned() })
    }

    fn confine(&self, rel: &Path, shown: &str) -> Result<PathBuf> {
        loa_util::fs::confine(&self.root, rel).map_err(|e| match e.kind() {
            io::ErrorKind::PermissionDenied => unsafe_entry(shown, "path leaves the payload"),
            _ => Error::Io { path: self.root.join(rel), source: e },
        })
    }

    fn dirs(&self, rel: &Path, shown: &str) -> Result<PathBuf> {
        let mut parent = self.root.clone();
        for part in rel.components() {
            parent.push(part);
            match fs::symlink_metadata(&parent) {
                Ok(meta) if meta.file_type().is_symlink() => return Err(unsafe_entry(shown, "entry parent is a symlink")),
                Ok(_) => {}
                Err(err) if err.kind() == io::ErrorKind::NotFound => break,
                Err(err) => return Err(Error::Io { path: parent, source: err }),
            }
        }
        let path = self.confine(rel, shown)?;
        fs::create_dir_all(&path).map_err(io_at(&path))?;
        Ok(path)
    }

    /// A fresh leaf at `rel`: parents made, an earlier file or link there removed.
    fn leaf(&self, rel: &Path, shown: &str) -> Result<PathBuf> {
        let name = rel.file_name().ok_or_else(|| unsafe_entry(shown, "no file name"))?;
        let dest = self.dirs(rel.parent().unwrap_or(Path::new("")), shown)?.join(name);
        match fs::symlink_metadata(&dest) {
            Ok(meta) if meta.is_dir() => Err(unsafe_entry(shown, "replaces a directory")),
            Ok(_) => fs::remove_file(&dest).map_err(io_at(&dest)).map(|()| dest),
            Err(_) => Ok(dest),
        }
    }

    fn file(&self, rel: &Path, shown: &str, data: &mut dyn Read, mode: u32) -> Result<()> {
        let dest = self.leaf(rel, shown)?;
        let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&dest).map_err(io_at(&dest))?;
        io::copy(data, &mut file).map_err(io_at(&dest))?;
        // Closed synced so no writable descriptor stays open once it may run.
        file.sync_all().map_err(io_at(&dest))?;
        drop(file);
        set_mode(&dest, (mode & 0o777) | 0o600)
    }

    /// A relative link that may only climb before it descends, and never above the payload.
    fn symlink(&self, rel: &Path, target: &Path, shown: &str) -> Result<()> {
        let mut depth = rel.parent().map_or(0, |p| p.components().count());
        let mut descended = false;
        for part in target.components() {
            match part {
                Component::ParentDir if descended => return Err(unsafe_entry(shown, "symlink target climbs after descending")),
                Component::ParentDir => depth = depth.checked_sub(1).ok_or_else(|| unsafe_entry(shown, "symlink points outside the payload"))?,
                Component::Normal(_) => descended = true,
                Component::CurDir => {}
                _ => return Err(unsafe_entry(shown, "symlink target is absolute")),
            }
        }
        if target.as_os_str().is_empty() {
            return Err(unsafe_entry(shown, "symlink without target"));
        }
        let dest = self.leaf(rel, shown)?;
        #[cfg(unix)]
        return std::os::unix::fs::symlink(target, &dest).map_err(io_at(&dest));
        #[cfg(not(unix))]
        Err(unsafe_entry(shown, "symlinks are unsupported here"))
    }

    fn hard_link(&self, rel: &Path, target: &Path, shown: &str) -> Result<()> {
        let source = self.confine(target, shown)?;
        if !fs::symlink_metadata(&source).is_ok_and(|m| m.is_file()) {
            return Err(unsafe_entry(shown, "hard link target is not a regular file"));
        }
        let dest = self.leaf(rel, shown)?;
        fs::hard_link(&source, &dest).map_err(io_at(&dest))
    }
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_at(path))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<()> {
    Ok(())
}
