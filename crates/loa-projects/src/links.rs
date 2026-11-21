//! A project's `.omgui` folder, inside the project's folder: one symlink per directory its sessions run in, so the
//! project's work is one folder away wherever it lives. A `.gitignore` of `*` keeps the links out of git.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Name of the links folder inside a project's folder.
pub const LINKS_DIR: &str = ".omgui";

pub(crate) const GITIGNORE: &str = ".gitignore";

/// Makes `<folder>/.omgui` hold exactly one link to each of `dirs` that exists. The folder itself gets no link: it
/// would point into its own parent and loop. A link is named after its directory, with `-2`, `-3`, … on a clash.
/// Links to other directories are removed; anything else in the folder is left alone. With no links left the
/// folder is removed.
pub fn sync(folder: &Path, dirs: &BTreeSet<PathBuf>) -> io::Result<()> {
    let links = folder.join(LINKS_DIR);
    let mut wanted: BTreeSet<&Path> = dirs.iter().map(PathBuf::as_path).filter(|dir| *dir != folder && dir.is_dir()).collect();
    let mut taken: BTreeSet<OsString> = BTreeSet::new();
    if links.is_dir() {
        for entry in std::fs::read_dir(&links)? {
            let entry = entry?;
            // Anything that isn't a link stays, and so does a link to a wanted directory.
            if !entry.file_type()?.is_symlink() || wanted.remove(std::fs::read_link(entry.path())?.as_path()) {
                taken.insert(entry.file_name());
            } else {
                remove_link(&entry.path())?;
            }
        }
    }
    if wanted.is_empty() && taken.iter().all(|name| name == GITIGNORE) {
        return remove(folder);
    }
    std::fs::create_dir_all(&links)?;
    let ignore = links.join(GITIGNORE);
    if !ignore.exists() {
        std::fs::write(&ignore, "*\n")?;
        taken.insert(GITIGNORE.into());
    }
    for dir in wanted {
        let base = dir.file_name().map_or_else(|| OsString::from("root"), OsString::from);
        let mut name = base.clone();
        let mut n = 2;
        while taken.contains(&name) {
            name = base.clone();
            name.push(format!("-{n}"));
            n += 1;
        }
        symlink_dir(dir, &links.join(&name))?;
        taken.insert(name);
    }
    Ok(())
}

/// Removes `<folder>/.omgui`'s links and `.gitignore`, then the folder when nothing else is in it.
pub fn remove(folder: &Path) -> io::Result<()> {
    let links = folder.join(LINKS_DIR);
    if !links.is_dir() {
        return Ok(());
    }
    for entry in std::fs::read_dir(&links)? {
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            remove_link(&entry.path())?;
        } else if entry.file_name() == GITIGNORE {
            std::fs::remove_file(entry.path())?;
        }
    }
    match std::fs::remove_dir(&links) {
        // Something of the user's is still in it.
        Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(()),
        result => result,
    }
}

#[cfg(unix)]
fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink_dir(target: &Path, link: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
}

/// Unix links are files; Windows directory links are removed as directories.
fn remove_link(link: &Path) -> io::Result<()> {
    std::fs::remove_file(link).or_else(|_| std::fs::remove_dir(link))
}
