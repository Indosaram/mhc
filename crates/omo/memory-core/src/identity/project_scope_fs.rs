//! Per-session asynchronous filesystem memo. Concurrent probes share one initialization.
use super::git_layout::{GitEntryKind, GitLayoutProbe};
use crate::fs::resilient;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::OnceCell;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Directory,
    File,
    Symlink,
}
type Memo<T> = Mutex<HashMap<PathBuf, Arc<OnceCell<Option<T>>>>>;
#[derive(Default)]
pub struct ProjectScopeProbe {
    real: Memo<PathBuf>,
    kinds: Memo<EntryKind>,
    stats: Memo<EntryKind>,
    text: Memo<String>,
    names: Memo<Vec<String>>,
}
fn cell<T>(memo: &Memo<T>, path: &Path) -> Arc<OnceCell<Option<T>>> {
    memo.lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(path.to_path_buf())
        .or_default()
        .clone()
}
fn kind(m: std::fs::Metadata) -> Option<EntryKind> {
    if m.is_symlink() {
        Some(EntryKind::Symlink)
    } else if m.is_dir() {
        Some(EntryKind::Directory)
    } else if m.is_file() {
        Some(EntryKind::File)
    } else {
        None
    }
}
impl ProjectScopeProbe {
    pub fn new() -> Self {
        Self::default()
    }
    pub async fn real_path(&self, path: &Path) -> Option<PathBuf> {
        let p = path.to_path_buf();
        cell(&self.real, path)
            .get_or_init(|| async move {
                tokio::task::spawn_blocking(move || resilient::canonicalize(&p).ok())
                    .await
                    .ok()
                    .flatten()
            })
            .await
            .clone()
    }
    pub async fn link_kind(&self, path: &Path) -> Option<EntryKind> {
        let p = path.to_path_buf();
        *cell(&self.kinds, path)
            .get_or_init(|| async move {
                tokio::task::spawn_blocking(move || {
                    resilient::symlink_metadata(&p).ok().and_then(kind)
                })
                .await
                .ok()
                .flatten()
            })
            .await
    }
    pub async fn stat_kind(&self, path: &Path) -> Option<EntryKind> {
        let p = path.to_path_buf();
        *cell(&self.stats, path)
            .get_or_init(|| async move {
                tokio::task::spawn_blocking(move || resilient::metadata(&p).ok().and_then(kind))
                    .await
                    .ok()
                    .flatten()
            })
            .await
    }
    pub async fn read_text(&self, path: &Path) -> Option<String> {
        let p = path.to_path_buf();
        cell(&self.text, path)
            .get_or_init(|| async move {
                tokio::task::spawn_blocking(move || resilient::read_to_string(&p).ok())
                    .await
                    .ok()
                    .flatten()
            })
            .await
            .clone()
    }
    pub async fn readdir(&self, path: &Path) -> Option<Vec<String>> {
        let p = path.to_path_buf();
        cell(&self.names, path)
            .get_or_init(|| async move {
                tokio::task::spawn_blocking(move || resilient::read_dir_names(&p).ok())
                    .await
                    .ok()
                    .flatten()
            })
            .await
            .clone()
    }
    pub async fn exists(&self, path: &Path) -> bool {
        self.stat_kind(path).await.is_some()
    }
    /// Preload the finite git-layout walk so the injectable synchronous parser reads memo only.
    pub async fn prepare_git_layout(&self, start: &Path) {
        for dir in start.ancestors() {
            let marker = dir.join(".git");
            match self.stat_kind(&marker).await {
                Some(EntryKind::Directory) => break,
                Some(EntryKind::File) => {
                    if let Some(text) = self.read_text(&marker).await
                        && let Some(target) = text
                            .lines()
                            .next()
                            .and_then(|s| s.trim().strip_prefix("gitdir:"))
                            .map(str::trim)
                        {
                            let admin = crate::support::paths::resolve_from(dir, Path::new(target));
                            let back = self.read_text(&admin.join("gitdir")).await;
                            self.real_path(&marker).await;
                            if let Some(back) =
                                back.and_then(|s| s.lines().next().map(|s| s.trim().to_owned()))
                            {
                                self.real_path(&crate::support::paths::resolve_from(
                                    &admin,
                                    Path::new(&back),
                                ))
                                .await;
                            }
                            if let Some(common) = self
                                .read_text(&admin.join("commondir"))
                                .await
                                .and_then(|s| s.lines().next().map(|s| s.trim().to_owned()))
                            {
                                self.read_text(
                                    &crate::support::paths::resolve_from(
                                        &admin,
                                        Path::new(&common),
                                    )
                                    .join("config"),
                                )
                                .await;
                            }
                        }
                    break;
                }
                _ => {}
            }
        }
    }
}
fn cached<T: Clone>(memo: &Memo<T>, path: &Path) -> Option<T> {
    memo.lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(path)
        .and_then(|v| v.get())
        .cloned()
        .flatten()
}
impl GitLayoutProbe for ProjectScopeProbe {
    fn entry_kind(&self, path: &Path) -> Option<GitEntryKind> {
        match cached(&self.stats, path) {
            Some(EntryKind::Directory) => Some(GitEntryKind::Directory),
            Some(EntryKind::File) => Some(GitEntryKind::File),
            _ => None,
        }
    }
    fn read_text(&self, path: &Path) -> Option<String> {
        cached(&self.text, path)
    }
    fn real_path(&self, path: &Path) -> Option<PathBuf> {
        cached(&self.real, path)
    }
}
pub fn create_project_scope_probe() -> ProjectScopeProbe {
    ProjectScopeProbe::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn settled_answers_are_replayed_after_files_change() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("note");
        resilient::write(&file, b"first").unwrap();
        let probe = ProjectScopeProbe::new();
        assert_eq!(probe.read_text(&file).await.as_deref(), Some("first"));
        resilient::write(&file, b"second").unwrap();
        assert_eq!(probe.read_text(&file).await.as_deref(), Some("first"));
        assert!(probe.read_text(&dir.path().join("missing")).await.is_none());
        resilient::write(&dir.path().join("missing"), b"late").unwrap();
        assert!(probe.read_text(&dir.path().join("missing")).await.is_none());
    }
}
