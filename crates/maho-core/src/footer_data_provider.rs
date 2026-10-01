//! Port of senpi packages/coding-agent/src/core/footer-data-provider.ts (the data half).
//!
//! senpi watches .git/HEAD with fs.watchFile and a retry timer. The Rust port keeps the same state
//! machine but exposes refresh_branch() for the host's event loop to call: maho-tui owns terminal
//! polling, so no watcher thread or fixed sleep lives here.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPaths {
    pub repo_dir: String,
    pub common_git_dir: String,
    pub head_path: String,
}

/// Finds git metadata paths by walking up from cwd. Handles regular repos (.git is a directory)
/// and worktrees (.git is a file pointing at a gitdir).
pub fn find_git_paths(cwd: &str) -> Option<GitPaths> {
    let mut dir = PathBuf::from(cwd);
    loop {
        let git_path = dir.join(".git");
        if git_path.exists() {
            match std::fs::metadata(&git_path) {
                Ok(metadata) if metadata.is_file() => {
                    let Ok(content) = std::fs::read_to_string(&git_path) else { return None };
                    let content = content.trim();
                    if let Some(rest) = content.strip_prefix("gitdir: ") {
                        let git_dir = resolve_against(&dir, rest.trim());
                        let head_path = git_dir.join("HEAD");
                        if !head_path.exists() {
                            return None;
                        }
                        let common_dir_path = git_dir.join("commondir");
                        let common_git_dir = match std::fs::read_to_string(&common_dir_path) {
                            Ok(common) => resolve_against(&git_dir, common.trim()),
                            Err(_) => git_dir.clone(),
                        };
                        return Some(GitPaths {
                            repo_dir: dir.to_string_lossy().into_owned(),
                            common_git_dir: common_git_dir.to_string_lossy().into_owned(),
                            head_path: head_path.to_string_lossy().into_owned(),
                        });
                    }
                    return None;
                }
                Ok(metadata) if metadata.is_dir() => {
                    let head_path = git_path.join("HEAD");
                    if !head_path.exists() {
                        return None;
                    }
                    return Some(GitPaths {
                        repo_dir: dir.to_string_lossy().into_owned(),
                        common_git_dir: git_path.to_string_lossy().into_owned(),
                        head_path: head_path.to_string_lossy().into_owned(),
                    });
                }
                _ => return None,
            }
        }
        match dir.parent() {
            Some(parent) if parent != dir => dir = parent.to_path_buf(),
            _ => return None,
        }
    }
}

fn resolve_against(base: &Path, relative: &str) -> PathBuf {
    let path = Path::new(relative);
    if path.is_absolute() { path.to_path_buf() } else { base.join(path) }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadBranch {
    pub branch: Option<String>,
    /// A reftable HEAD only git can resolve.
    pub needs_git_probe: bool,
}

fn is_wsl_environment() -> bool {
    cfg!(target_os = "linux")
        && (std::env::var_os("WSL_DISTRO_NAME").is_some() || std::env::var_os("WSL_INTEROP").is_some())
}

fn is_windows_mounted_repo_path(repo_dir: &str) -> bool {
    let lowered = repo_dir.to_ascii_lowercase();
    lowered.starts_with("/mnt/") && lowered.as_bytes().get(5).map(|byte| byte.is_ascii_lowercase()).unwrap_or(false)
}

fn should_poll_git_head(repo_dir: &str) -> bool {
    is_wsl_environment() && is_windows_mounted_repo_path(repo_dir)
}

/// Provides git branch and extension statuses - data not otherwise accessible to extensions.
pub struct FooterDataProvider {
    cwd: Mutex<String>,
    extension_statuses: Mutex<BTreeMap<String, String>>,
    cached_branch: Mutex<Option<Option<String>>>,
    git_paths: Mutex<Option<Option<GitPaths>>>,
    branch_change_callbacks: Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>,
    available_provider_count: Mutex<usize>,
    disposed: Mutex<bool>,
}

impl FooterDataProvider {
    pub fn new(cwd: &str) -> Self {
        let provider = Self {
            cwd: Mutex::new(cwd.to_owned()),
            extension_statuses: Mutex::new(BTreeMap::new()),
            cached_branch: Mutex::new(None),
            git_paths: Mutex::new(Some(find_git_paths(cwd))),
            branch_change_callbacks: Mutex::new(Vec::new()),
            available_provider_count: Mutex::new(0),
            disposed: Mutex::new(false),
        };
        provider
    }

    fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        value.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Current git branch: None outside a repo, "detached" on a detached HEAD.
    pub fn git_branch(&self) -> Option<String> {
        let mut cached = Self::lock(&self.cached_branch);
        if cached.is_none() {
            *cached = Some(self.read_branch_from_head().branch);
        }
        cached.clone().flatten()
    }

    pub fn extension_statuses(&self) -> BTreeMap<String, String> {
        Self::lock(&self.extension_statuses).clone()
    }

    pub fn on_branch_change(&self, callback: Arc<dyn Fn() + Send + Sync>) {
        Self::lock(&self.branch_change_callbacks).push(callback);
    }

    pub fn set_extension_status(&self, key: &str, text: Option<&str>) {
        let mut statuses = Self::lock(&self.extension_statuses);
        match text {
            Some(text) => {
                statuses.insert(key.to_owned(), text.to_owned());
            }
            None => {
                statuses.remove(key);
            }
        }
    }

    pub fn clear_extension_statuses(&self) {
        Self::lock(&self.extension_statuses).clear();
    }

    pub fn available_provider_count(&self) -> usize {
        *Self::lock(&self.available_provider_count)
    }

    pub fn set_available_provider_count(&self, count: usize) {
        *Self::lock(&self.available_provider_count) = count;
    }

    pub fn set_cwd(&self, cwd: &str) {
        {
            let mut current = Self::lock(&self.cwd);
            if *current == cwd {
                return;
            }
            *current = cwd.to_owned();
        }
        *Self::lock(&self.cached_branch) = None;
        *Self::lock(&self.git_paths) = Some(find_git_paths(cwd));
        self.notify_branch_change();
    }

    pub fn dispose(&self) {
        *Self::lock(&self.disposed) = true;
        Self::lock(&self.branch_change_callbacks).clear();
    }

    fn notify_branch_change(&self) {
        for callback in Self::lock(&self.branch_change_callbacks).iter() {
            callback();
        }
    }

    fn read_branch_from_head(&self) -> HeadBranch {
        let paths = Self::lock(&self.git_paths).clone();
        let Some(Some(paths)) = paths else { return HeadBranch { branch: None, needs_git_probe: false } };
        let Ok(content) = std::fs::read_to_string(&paths.head_path) else {
            return HeadBranch { branch: None, needs_git_probe: false };
        };
        let content = content.trim();
        let Some(branch) = content.strip_prefix("ref: refs/heads/") else {
            return HeadBranch { branch: Some("detached".to_owned()), needs_git_probe: false };
        };
        if branch == ".invalid" {
            HeadBranch { branch: Some("detached".to_owned()), needs_git_probe: true }
        } else {
            HeadBranch { branch: Some(branch.to_owned()), needs_git_probe: false }
        }
    }

    /// Asks git for the current branch; None on a detached HEAD or when git is unavailable.
    pub fn resolve_git_branch(&self) -> Option<String> {
        let repo_dir = Self::lock(&self.git_paths).clone().flatten().map(|paths| paths.repo_dir)?;
        let output = std::process::Command::new("git")
            .args(["--no-optional-locks", "symbolic-ref", "--quiet", "--short", "HEAD"])
            .current_dir(&repo_dir)
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if branch.is_empty() { None } else { Some(branch) }
    }

    /// Re-resolves the branch and notifies subscribers when it changed. The host calls this from its
    /// event loop; senpi's fs.watchFile debounce is the caller's cadence, not a sleep here.
    pub fn refresh_branch(&self) {
        if *Self::lock(&self.disposed) {
            return;
        }
        let needs_probe = self.read_branch_from_head().needs_git_probe;
        let next = if needs_probe { self.resolve_git_branch() } else { self.read_branch_from_head().branch };
        let mut cached = Self::lock(&self.cached_branch);
        let previous = cached.clone();
        let changed = previous.is_some() && previous.clone().flatten() != next;
        *cached = Some(next);
        drop(cached);
        if changed {
            self.notify_branch_change();
        }
    }

    /// Whether the repo needs a git probe rather than a HEAD read (WSL over a Windows mount).
    pub fn should_poll_git_head(&self) -> bool {
        Self::lock(&self.git_paths)
            .clone()
            .flatten()
            .map(|paths| should_poll_git_head(&paths.repo_dir))
            .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn init_repo(dir: &Path) {
        std::process::Command::new("git").args(["init", "-q", "-b", "feature"]).current_dir(dir).output().expect("git init");
    }

    #[test]
    fn a_plain_repo_is_found_by_walking_up() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_repo(tmp.path());
        std::fs::create_dir_all(tmp.path().join("nested/deep")).expect("mkdir");
        let paths = find_git_paths(&tmp.path().join("nested/deep").to_string_lossy()).expect("paths");
        assert_eq!(paths.repo_dir, tmp.path().to_string_lossy());
        assert!(paths.head_path.ends_with(".git/HEAD"));
    }

    #[test]
    fn a_missing_repo_yields_none() {
        assert!(find_git_paths("/definitely/not/a/repo").is_none());
    }

    #[test]
    fn the_branch_is_read_from_head() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_repo(tmp.path());
        let provider = FooterDataProvider::new(&tmp.path().to_string_lossy());
        assert_eq!(provider.git_branch().as_deref(), Some("feature"));
    }

    #[test]
    fn a_detached_head_reports_detached() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_repo(tmp.path());
        std::fs::write(tmp.path().join(".git/HEAD"), "0123456789abcdef
").expect("write");
        let provider = FooterDataProvider::new(&tmp.path().to_string_lossy());
        assert_eq!(provider.git_branch().as_deref(), Some("detached"));
    }

    #[test]
    fn extension_statuses_track_set_and_clear() {
        let provider = FooterDataProvider::new("/tmp");
        provider.set_extension_status("k", Some("v"));
        assert_eq!(provider.extension_statuses().get("k").map(String::as_str), Some("v"));
        provider.set_extension_status("k", None);
        assert!(provider.extension_statuses().is_empty());
    }

    #[test]
    fn a_branch_change_notifies_subscribers() {
        let tmp = tempfile::tempdir().expect("tempdir");
        init_repo(tmp.path());
        let provider = FooterDataProvider::new(&tmp.path().to_string_lossy());
        let _ = provider.git_branch();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        provider.on_branch_change(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        std::fs::write(tmp.path().join(".git/HEAD"), "ref: refs/heads/renamed
").expect("write");
        provider.refresh_branch();
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        assert_eq!(provider.git_branch().as_deref(), Some("renamed"));
    }
}
