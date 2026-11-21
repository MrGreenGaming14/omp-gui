//! Isolated checkouts and their changes relative to the opening snapshot.
//!
//! A [`CheckoutBackend`] makes the checkout: a `git worktree`, a clone where
//! worktrees can't be made, or ovnfs (feature `ovnfs`) when it is installed.
//! No Git command here changes the source index, HEAD or working tree; the
//! checkout owns its own index.

use crate::{Result, SandboxError};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

fn error(message: impl Into<String>) -> SandboxError {
    SandboxError::Isolation(message.into())
}

/// Runs `cmd` and returns its stdout, or fails with its stderr.
fn output(cmd: &mut Command) -> Result<Vec<u8>> {
    let out = cmd.output()?;
    if !out.status.success() {
        let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy()).collect();
        return Err(error(format!("{} {}: {}", cmd.get_program().to_string_lossy(), args.join(" "), String::from_utf8_lossy(&out.stderr).trim())));
    }
    Ok(out.stdout)
}

fn git(dir: &Path) -> Command {
    let mut cmd = Command::new("git");
    cmd.current_dir(dir);
    cmd
}

fn toplevel(dir: &Path) -> Result<PathBuf> {
    let out = output(git(dir).args(["rev-parse", "--show-toplevel"]))?;
    Ok(PathBuf::from(String::from_utf8_lossy(&out).trim_end()))
}

/// Makes and removes checkouts of a repository's HEAD.
pub trait CheckoutBackend: Send + Sync {
    fn name(&self) -> &'static str;
    /// Makes the checkout `name` of `repo` and returns its root.
    fn create(&self, repo: &Path, name: &str) -> Result<PathBuf>;
    fn remove(&self, repo: &Path, name: &str, checkout: &Path) -> Result<()>;
}

pub struct GitWorktree;
pub struct GitClone;

impl CheckoutBackend for GitWorktree {
    fn name(&self) -> &'static str { "git worktree" }
    fn create(&self, repo: &Path, name: &str) -> Result<PathBuf> {
        let dst = std::env::temp_dir().join(name);
        output(git(repo).args(["worktree", "add", "--detach", "--"]).arg(&dst).arg("HEAD"))?;
        Ok(dst)
    }
    fn remove(&self, repo: &Path, _: &str, checkout: &Path) -> Result<()> {
        output(git(repo).args(["worktree", "remove", "--force", "--"]).arg(checkout)).map(drop)
    }
}

/// A clone has a private Git directory, so it can be made where a linked worktree can't.
impl CheckoutBackend for GitClone {
    fn name(&self) -> &'static str { "git clone" }
    fn create(&self, repo: &Path, name: &str) -> Result<PathBuf> {
        let dst = std::env::temp_dir().join(name);
        output(git(&std::env::temp_dir()).args(["clone", "--no-hardlinks", "--"]).arg(repo).arg(&dst))?;
        Ok(dst)
    }
    fn remove(&self, _: &Path, _: &str, checkout: &Path) -> Result<()> { Ok(fs::remove_dir_all(checkout)?) }
}

/// An ovnfs checkout, which owns its Git directory too.
#[cfg(feature = "ovnfs")]
pub struct Ovnfs;

#[cfg(feature = "ovnfs")]
impl CheckoutBackend for Ovnfs {
    fn name(&self) -> &'static str { "ovnfs" }
    fn create(&self, repo: &Path, name: &str) -> Result<PathBuf> {
        let root = PathBuf::from(String::from_utf8_lossy(&output(Command::new("ovnfs").args(["new", "--name", name]).current_dir(repo))?).trim());
        if !root.is_dir() {
            let _ = self.remove(repo, name, &root);
            return Err(error("ovnfs returned no checkout directory"));
        }
        Ok(root)
    }
    fn remove(&self, repo: &Path, name: &str, _: &Path) -> Result<()> {
        output(Command::new("ovnfs").args(["rm", name]).current_dir(repo)).map(drop)
    }
}

/// The backends to try, in order. An installed ovnfs is authoritative: its
/// failure must not silently degrade to another isolation mechanism.
fn backends() -> &'static [&'static dyn CheckoutBackend] {
    #[cfg(feature = "ovnfs")]
    if loa_util::exec::which("ovnfs").is_some() {
        return &[&Ovnfs];
    }
    &[&GitWorktree, &GitClone]
}

/// The isolation [`IsolatedCheckout::create`] uses on this machine.
pub fn backend_name() -> &'static str {
    backends()[0].name()
}

/// An isolated checkout, removed when dropped. [`IsolatedCheckout::patch`]
/// includes unstaged, staged and new files relative to its opening snapshot.
pub struct IsolatedCheckout {
    repo: PathBuf,
    name: String,
    root: PathBuf,
    path: PathBuf,
    baseline: String,
    backend: &'static dyn CheckoutBackend,
}

impl IsolatedCheckout {
    pub fn create(workspace: &Path) -> Result<Self> {
        let source = workspace.canonicalize()?;
        let repo = toplevel(&source)?;
        let relative = source.strip_prefix(&repo).map_err(|e| error(e.to_string()))?.to_path_buf();
        let name = format!("loa-sandbox-{}-{:x}-{}", std::process::id(), loa_util::time::now_ms(), NEXT.fetch_add(1, Ordering::Relaxed));
        let mut failure = error("no checkout backend");
        for &backend in backends() {
            match backend.create(&repo, &name) {
                Ok(root) => {
                    let mut checkout = Self { path: root.join(&relative), root, repo, name, baseline: String::new(), backend };
                    overlay(&checkout.repo, &checkout.root)?;
                    checkout.baseline = String::from_utf8_lossy(&checkout.stage(&["write-tree"])?).trim().to_owned();
                    return Ok(checkout);
                }
                Err(e) => failure = e,
            }
        }
        Err(failure)
    }

    pub fn path(&self) -> &Path { &self.path }

    /// Stages everything in the checkout, then runs `git args` there.
    fn stage(&self, args: &[&str]) -> Result<Vec<u8>> {
        output(git(&self.root).args(["add", "-A", "--", "."]))?;
        output(git(&self.root).args(args))
    }

    /// A binary capable unified Git patch of the changes since the checkout was made.
    pub fn patch(&self) -> Result<Vec<u8>> {
        self.stage(&["diff", "--cached", "--binary", "--no-ext-diff", &self.baseline, "--"])
    }

    fn remove(&self) -> Result<()> {
        if self.root.exists() { self.backend.remove(&self.repo, &self.name, &self.root) } else { Ok(()) }
    }
}

impl Drop for IsolatedCheckout {
    fn drop(&mut self) { let _ = self.remove(); }
}

/// Replaces `dst` with a copy of `src` (a file, symlink or directory tree).
fn copy_entry(src: &Path, dst: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)?;
    remove_entry(dst)?;
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent)?;
    }
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(src)?, dst)?;
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

pub(crate) fn remove_entry(path: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(_) => Ok(()),
    }
}

/// Overlays the tracked and untracked source files onto the clean checkout;
/// files Git lists but the source deleted are removed from it.
fn overlay(source: &Path, checkout: &Path) -> Result<()> {
    let files = output(git(source).args(["ls-files", "--cached", "--others", "--exclude-standard", "-z"]))?;
    for path in files.split(|b| *b == 0).filter(|path| !path.is_empty()).map(|p| Path::new(std::ffi::OsStr::from_bytes(p))) {
        let (from, to) = (source.join(path), checkout.join(path));
        if fs::symlink_metadata(&from).is_ok() { copy_entry(&from, &to)? } else { remove_entry(&to)? }
    }
    Ok(())
}

/// Applies a patch from [`IsolatedCheckout::patch`] to the repository that
/// holds `workspace`. Checks the whole patch first, so a patch that doesn't
/// apply cleanly changes nothing.
pub fn apply_patch(workspace: &Path, patch: &[u8]) -> Result<()> {
    let root = toplevel(workspace)?;
    for check in [true, false] {
        let mut cmd = git(&root);
        cmd.arg("apply").args(check.then_some("--check")).args(["--binary", "-"]);
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        std::io::Write::write_all(&mut child.stdin.take().expect("piped stdin"), patch)?;
        let out = child.wait_with_output()?;
        if !out.status.success() {
            return Err(error(format!("git apply: {}", String::from_utf8_lossy(&out.stderr).trim())));
        }
    }
    Ok(())
}

/// One file a patch touches.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatchFile {
    pub path: String,
    pub added: usize,
    pub removed: usize,
    pub binary: bool,
}

/// The files of a unified Git patch with their added and removed line counts.
pub fn patch_files(patch: &str) -> Vec<PatchFile> {
    let mut files: Vec<PatchFile> = Vec::new();
    let mut in_hunk = false;
    for line in patch.lines() {
        if let Some(header) = line.strip_prefix("diff --git ") {
            // `a/<path> b/<path>`; the new name is what the change produces.
            let path = header.rsplit_once(" b/").map_or(header, |(_, new)| new);
            files.push(PatchFile { path: path.to_owned(), added: 0, removed: 0, binary: false });
            in_hunk = false;
            continue;
        }
        let Some(file) = files.last_mut() else { continue };
        match line.as_bytes().first() {
            _ if line.starts_with("@@") => in_hunk = true,
            _ if line == "GIT binary patch" || (line.starts_with("Binary files ") && line.ends_with(" differ")) => file.binary = true,
            Some(b'+') if in_hunk => file.added += 1,
            Some(b'-') if in_hunk => file.removed += 1,
            _ => {}
        }
    }
    files
}
