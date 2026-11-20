//! A project's `.omgui` folder, inside the project's folder: one symlink per
//! directory its sessions run in, so the project's work is one folder away
//! wherever it lives. A `.gitignore` of `*` keeps the links out of git.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Name of the links folder inside a project's folder.
pub const LINKS_DIR: &str = ".omgui";

const GITIGNORE: &str = ".gitignore";

/// Makes `<folder>/.omgui` hold exactly one link to each of `dirs` that
/// exists. The folder itself gets no link: it would point into its own
/// parent and loop. A link is named after its directory, with `-2`, `-3`, …
/// on a clash. Links to other directories are removed; anything else in the
/// folder is left alone. With no links left the folder is removed.
pub fn sync(folder: &Path, dirs: &BTreeSet<PathBuf>) -> io::Result<()> {
    let links = folder.join(LINKS_DIR);
    let mut wanted: BTreeSet<&Path> =
        dirs.iter().map(PathBuf::as_path).filter(|dir| *dir != folder && dir.is_dir()).collect();
    let mut taken: BTreeSet<OsString> = BTreeSet::new();
    if links.is_dir() {
        for entry in std::fs::read_dir(&links)? {
            let entry = entry?;
            if entry.file_type()?.is_symlink() {
                let target = std::fs::read_link(entry.path())?;
                if wanted.remove(target.as_path()) {
                    taken.insert(entry.file_name());
                } else {
                    remove_link(&entry.path())?;
                }
            } else {
                taken.insert(entry.file_name());
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

/// Removes `<folder>/.omgui`'s links and `.gitignore`, then the folder when
/// nothing else is in it.
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn names(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(folder.join(LINKS_DIR))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn links_follow_the_session_directories() {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        let (api, other_api, web) = (root.path().join("a/api"), root.path().join("b/api"), root.path().join("web"));
        for dir in [&project, &api, &other_api, &web] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let missing = root.path().join("gone");

        let dirs = BTreeSet::from([project.clone(), api.clone(), other_api.clone(), missing]);
        sync(&project, &dirs).unwrap();
        assert_eq!(names(&project), [".gitignore", "api", "api-2"], "no self link, no missing dir, clashes numbered");
        assert_eq!(std::fs::read_to_string(project.join(LINKS_DIR).join(GITIGNORE)).unwrap(), "*\n");
        let api_link = std::fs::read_link(project.join(LINKS_DIR).join("api")).unwrap();
        assert!(api_link == api || api_link == other_api);

        sync(&project, &BTreeSet::from([api.clone(), web.clone()])).unwrap();
        assert_eq!(names(&project).len(), 3, "{:?}", names(&project));
        assert!(names(&project).contains(&"web".to_owned()));
        let linked: BTreeSet<PathBuf> = std::fs::read_dir(project.join(LINKS_DIR))
            .unwrap()
            .filter_map(|entry| std::fs::read_link(entry.unwrap().path()).ok())
            .collect();
        assert_eq!(linked, BTreeSet::from([api, web]), "the dropped directory's link is gone");

        sync(&project, &BTreeSet::new()).unwrap();
        assert!(!project.join(LINKS_DIR).exists(), "no sessions left: no folder");
    }

    #[test]
    fn remove_keeps_the_users_own_files() {
        let root = tempfile::tempdir().unwrap();
        let (project, api) = (root.path().join("project"), root.path().join("api"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&api).unwrap();
        sync(&project, &BTreeSet::from([api.clone()])).unwrap();
        std::fs::write(project.join(LINKS_DIR).join("notes.md"), "mine").unwrap();
        remove(&project).unwrap();
        assert_eq!(names(&project), ["notes.md"]);
        assert!(api.is_dir(), "the linked directory itself is untouched");
    }
}
