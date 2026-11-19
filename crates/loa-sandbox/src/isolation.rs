//! Isolated workspaces and changes relative to their opening snapshot.
//!
//! No Git command here changes the source index, HEAD, or working tree. The
//! checkout owns its own index; an ovnfs checkout owns its Git directory too.

use crate::{Result, SandboxError};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn error(message: impl Into<String>) -> SandboxError {
    SandboxError::Isolation(message.into())
}

fn command(program: &str, args: &[&str], dir: &Path) -> Result<Output> {
    let output = Command::new(program).args(args).current_dir(dir).output()?;
    if !output.status.success() {
        return Err(error(format!("{program} {}: {}", args.join(" "), String::from_utf8_lossy(&output.stderr))));
    }
    Ok(output)
}

fn git(args: &[&str], dir: &Path) -> Result<Output> {
    command("git", args, dir)
}

fn unique_dir() -> Result<PathBuf> {
    for _ in 0..100 {
        let count = NEXT.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| error(e.to_string()))?.as_nanos();
        let path = std::env::temp_dir().join(format!("loa-sandbox-{}-{now:x}-{count:x}", std::process::id()));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(error("could not allocate isolated workspace"))
}

fn copy_entry(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::symlink_metadata(dst).is_ok() {
        if fs::symlink_metadata(dst)?.is_dir() {
            fs::remove_dir_all(dst)?;
        } else {
            fs::remove_file(dst)?;
        }
    }
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
        #[cfg(not(unix))]
        return Err(error("symlink isolation requires Unix"));
    } else if meta.is_dir() {
        fs::create_dir(dst)?;
        for entry in fs::read_dir(src)? {
            let entry = entry?;
            copy_entry(&entry.path(), &dst.join(entry.file_name()))?;
        }
    } else if meta.is_file() {
        fs::copy(src, dst)?;
        fs::set_permissions(dst, meta.permissions())?;
    } else {
        return Err(error(format!("unsupported workspace entry: {}", src.display())));
    }
    Ok(())
}

/// Overlay the tracked and untracked source files onto a clean checkout.
/// Paths listed by Git but deleted in the source are removed from the copy.
fn overlay(source: &Path, checkout: &Path) -> Result<()> {
    let files = git(&["ls-files", "--cached", "--others", "--exclude-standard", "-z"], source)?.stdout;
    for path in files.split(|b| *b == 0).filter(|path| !path.is_empty()) {
        #[cfg(unix)]
        let relative = {
            use std::os::unix::ffi::OsStrExt;
            Path::new(std::ffi::OsStr::from_bytes(path))
        };
        #[cfg(not(unix))]
        let relative = Path::new(std::str::from_utf8(path).map_err(|e| error(e.to_string()))?);
        let from = source.join(relative);
        let to = checkout.join(relative);
        if fs::symlink_metadata(&from).is_ok() {
            copy_entry(&from, &to)?;
        } else if let Ok(meta) = fs::symlink_metadata(&to) {
            if meta.is_dir() { fs::remove_dir_all(&to)?; } else { fs::remove_file(&to)?; }
        }
    }
    Ok(())
}

fn has_ovnfs() -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| dir.join("ovnfs").is_file())
    })
}

enum Backend {
    Ovnfs(String),
    Worktree(PathBuf),
    Copy,
}

/// An isolated checkout, automatically cleaned up when dropped.
/// `patch` includes unstaged, staged and new files relative to its opening
/// snapshot, as a binary-capable unified Git patch.
pub struct IsolatedCheckout {
    path: PathBuf,
    baseline: String,
    backend: Backend,
    temporary: Option<PathBuf>,
}

impl IsolatedCheckout {
    pub fn create(workspace: &Path) -> Result<Self> {
        let source = workspace.canonicalize()?;
        if !source.is_dir() {
            return Err(error("workspace is not a directory"));
        }
        let root = git(&["rev-parse", "--show-toplevel"], &source)?;
        let root = PathBuf::from(String::from_utf8(root.stdout).map_err(|e| error(e.to_string()))?.trim_end());
        let relative = source.strip_prefix(&root).map_err(|e| error(e.to_string()))?;
        if has_ovnfs() {
            let name = format!("loa-{}-{:x}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
            // A present ovnfs is authoritative: failure must not silently
            // degrade to a different isolation mechanism.
            let output = command("ovnfs", &["new", "--name", &name], &root)?;
            let checkout_root = PathBuf::from(String::from_utf8(output.stdout).map_err(|e| error(e.to_string()))?.trim());
            if !checkout_root.is_dir() {
                let _ = command("ovnfs", &["rm", &name], &root);
                return Err(error("ovnfs returned no checkout directory"));
            }
            let initialized = (|| {
                overlay(&root, &checkout_root)?;
                Self::initialize(checkout_root.join(relative), Backend::Ovnfs(name.clone()), None)
            })();
            if initialized.is_err() {
                let _ = command("ovnfs", &["rm", &name], &root);
            }
            return initialized;
        }
        let temporary = unique_dir()?;
        let checkout_root = temporary.join("checkout");
        let worktree = Command::new("git").args(["worktree", "add", "--detach", "--", checkout_root.to_str().ok_or_else(|| error("non-UTF8 temporary path"))?, "HEAD"])
            .current_dir(&root).output()?;
        let backend = if worktree.status.success() {
            Backend::Worktree(root.clone())
        } else {
            // A clone has a private Git directory; unlike a linked worktree it
            // can still be made when worktree creation is unavailable.
            command("git", &["clone", "--no-hardlinks", "--", root.to_str().ok_or_else(|| error("non-UTF8 repository path"))?, checkout_root.to_str().ok_or_else(|| error("non-UTF8 temporary path"))?], &temporary)?;
            Backend::Copy
        };
        let result = (|| {
            overlay(&root, &checkout_root)?;
            Self::initialize(checkout_root.join(relative), backend, Some(temporary.clone()))
        })();
        if result.is_err() {
            let _ = git(&["worktree", "remove", "--force", "--", checkout_root.to_str().unwrap_or_default()], &root);
            let _ = fs::remove_dir_all(&temporary);
        }
        result
    }

    fn initialize(path: PathBuf, backend: Backend, temporary: Option<PathBuf>) -> Result<Self> {
        let root = git(&["rev-parse", "--show-toplevel"], &path)?;
        let root = PathBuf::from(String::from_utf8(root.stdout).map_err(|e| error(e.to_string()))?.trim_end());
        git(&["add", "-A", "--", "."], &root)?;
        let tree = git(&["write-tree"], &root)?;
        let baseline = String::from_utf8(tree.stdout).map_err(|e| error(e.to_string()))?.trim().to_owned();
        Ok(Self { path, baseline, backend, temporary })
    }

    pub fn path(&self) -> &Path { &self.path }

    pub fn patch(&self) -> Result<Vec<u8>> {
        let root = git(&["rev-parse", "--show-toplevel"], &self.path)?;
        let root = PathBuf::from(String::from_utf8(root.stdout).map_err(|e| error(e.to_string()))?.trim_end());
        git(&["add", "-A", "--", "."], &root)?;
        Ok(git(&["diff", "--cached", "--binary", "--no-ext-diff", &self.baseline, "--"], &root)?.stdout)
    }

    pub fn cleanup(self) -> Result<()> {
        self.remove()
    }

    fn remove(&self) -> Result<()> {
        match &self.backend {
            Backend::Ovnfs(name) => {
                if self.path.exists() {
                    command("ovnfs", &["rm", name], &self.path)?;
                }
            }
            Backend::Worktree(source) => {
                let checkout = self.temporary.as_ref().ok_or_else(|| error("checkout has no temporary directory"))?.join("checkout");
                if checkout.exists() {
                    git(&["worktree", "remove", "--force", "--", checkout.to_str().ok_or_else(|| error("non-UTF8 checkout path"))?], source)?;
                }
            }
            Backend::Copy => {}
        }
        if let Some(temporary) = &self.temporary
            && temporary.exists()
        {
            fs::remove_dir_all(temporary)?;
        }
        Ok(())
    }
}

impl Drop for IsolatedCheckout {
    fn drop(&mut self) { let _ = self.remove(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_keeps_original_and_includes_untracked() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        git(&["init", "-q"], root).unwrap();
        fs::write(root.join("tracked"), "before\n").unwrap();
        git(&["add", "tracked"], root).unwrap();
        git(&["-c", "user.name=Test", "-c", "user.email=t@example.org", "commit", "-qm", "baseline"], root).unwrap();
        fs::write(root.join("tracked"), "user edit\n").unwrap();
        fs::write(root.join("existing-untracked"), "already there\n").unwrap();
        let checkout = IsolatedCheckout::create(root).unwrap();
        assert_eq!(fs::read_to_string(checkout.path().join("tracked")).unwrap(), "user edit\n");
        fs::write(checkout.path().join("tracked"), "agent edit\n").unwrap();
        fs::write(checkout.path().join("new-untracked"), "new\n").unwrap();
        let patch = String::from_utf8(checkout.patch().unwrap()).unwrap();
        assert!(patch.contains("-user edit") && patch.contains("+agent edit"));
        assert!(patch.contains("new-untracked"));
        assert!(!patch.contains("existing-untracked"));
        assert_eq!(fs::read_to_string(root.join("tracked")).unwrap(), "user edit\n");
        assert!(!root.join("new-untracked").exists());
        checkout.cleanup().unwrap();
    }
}
