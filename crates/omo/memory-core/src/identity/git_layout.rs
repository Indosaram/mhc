//! Conservative linked-worktree discovery without launching git.
use crate::fs::resilient;
use crate::support::paths::resolve_from;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitEntryKind {
    Directory,
    File,
}
pub trait GitLayoutProbe {
    fn entry_kind(&self, path: &Path) -> Option<GitEntryKind>;
    fn read_text(&self, path: &Path) -> Option<String>;
    fn real_path(&self, path: &Path) -> Option<PathBuf>;
}
pub struct DefaultGitLayoutProbe;
impl GitLayoutProbe for DefaultGitLayoutProbe {
    fn entry_kind(&self, p: &Path) -> Option<GitEntryKind> {
        let m = resilient::metadata(p).ok()?;
        if m.is_dir() {
            Some(GitEntryKind::Directory)
        } else if m.is_file() {
            Some(GitEntryKind::File)
        } else {
            None
        }
    }
    fn read_text(&self, p: &Path) -> Option<String> {
        resilient::read_to_string(p).ok()
    }
    fn real_path(&self, p: &Path) -> Option<PathBuf> {
        resilient::canonicalize(p).ok()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedWorktreeSource {
    pub hash_source: PathBuf,
    pub slug_source: String,
}
fn name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}
fn first_line(text: Option<String>) -> Option<String> {
    text?.lines().next().map(|s| s.trim().to_owned())
}
pub fn resolve_linked_worktree_source(
    cwd: &Path,
    probe: &dyn GitLayoutProbe,
) -> Option<LinkedWorktreeSource> {
    let start = resolve_from(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        cwd,
    );
    for dir in start.ancestors() {
        let marker = dir.join(".git");
        match probe.entry_kind(&marker) {
            Some(GitEntryKind::Directory) => return None,
            Some(GitEntryKind::File) => {
                return from_pointer_file(&marker, dir, start.strip_prefix(dir).ok()?, probe);
            }
            None => {}
        }
    }
    None
}
fn from_pointer_file(
    marker: &Path,
    checkout: &Path,
    subpath: &Path,
    probe: &dyn GitLayoutProbe,
) -> Option<LinkedWorktreeSource> {
    let pointer = first_line(probe.read_text(marker))?;
    let target = pointer.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    let admin = resolve_from(checkout, Path::new(target));
    let common = first_line(probe.read_text(&admin.join("commondir")))?;
    if common.is_empty() || !points_back_to(&admin, marker, probe) {
        return None;
    }
    let (root, slug) = main_working_tree(&resolve_from(&admin, Path::new(&common)), probe)?;
    let hash_source = root.join(subpath);
    let slug_source = if subpath.as_os_str().is_empty() {
        slug
    } else {
        name(&hash_source)
    };
    Some(LinkedWorktreeSource {
        hash_source,
        slug_source,
    })
}
fn points_back_to(admin: &Path, marker: &Path, probe: &dyn GitLayoutProbe) -> bool {
    let Some(back) = first_line(probe.read_text(&admin.join("gitdir"))).filter(|s| !s.is_empty())
    else {
        return false;
    };
    let claimed = probe.real_path(&resolve_from(admin, Path::new(&back)));
    claimed.is_some() && claimed == probe.real_path(marker)
}
fn main_working_tree(common: &Path, probe: &dyn GitLayoutProbe) -> Option<(PathBuf, String)> {
    if common.file_name().is_some_and(|n| n == ".git") {
        let root = common.parent()?.to_path_buf();
        return Some((root.clone(), name(&root)));
    }
    let (bare, worktree) =
        read_core_config(&probe.read_text(&common.join("config")).unwrap_or_default());
    if let Some(worktree) = worktree.filter(|s| !s.is_empty()) {
        let root = resolve_from(common, Path::new(&worktree));
        return Some((root.clone(), name(&root)));
    }
    if bare == Some(true) {
        let n = name(common);
        let slug = n
            .strip_suffix(".git")
            .filter(|s| !s.is_empty())
            .unwrap_or(&n)
            .to_owned();
        return Some((common.to_path_buf(), slug));
    }
    None
}
fn read_core_config(text: &str) -> (Option<bool>, Option<String>) {
    let (mut bare, mut worktree, mut core) = (None, None, false);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            core = line
                .trim_start_matches('[')
                .split([']', ' ', '\t'])
                .next()
                .is_some_and(|s| s.eq_ignore_ascii_case("core"));
            continue;
        }
        if !core {
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let raw = raw.split_once(" #").map_or(raw, |(a, _)| a);
        let raw = raw.split_once(" ;").map_or(raw, |(a, _)| a);
        let value = raw.trim().trim_matches('"');
        if key.trim().eq_ignore_ascii_case("bare") {
            bare = Some(
                ["true", "yes", "on", "1"]
                    .iter()
                    .any(|s| value.eq_ignore_ascii_case(s)),
            );
        }
        if key.trim().eq_ignore_ascii_case("worktree") {
            worktree = Some(value.to_owned());
        }
    }
    (bare, worktree)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_back_pointer_and_preserves_subdirectory() {
        let t = tempfile::tempdir().unwrap();
        let main = t.path().join("main");
        let linked = t.path().join("linked");
        let admin = main.join(".git/worktrees/linked");
        std::fs::create_dir_all(&admin).unwrap();
        std::fs::create_dir_all(linked.join("src")).unwrap();
        std::fs::write(
            linked.join(".git"),
            format!("gitdir: {}\r\n", admin.display()),
        )
        .unwrap();
        std::fs::write(admin.join("commondir"), "../..\r\n").unwrap();
        std::fs::write(
            admin.join("gitdir"),
            linked.join(".git").to_string_lossy().as_bytes(),
        )
        .unwrap();
        let source =
            resolve_linked_worktree_source(&linked.join("src"), &DefaultGitLayoutProbe).unwrap();
        assert_eq!(source.hash_source, main.join("src"));
        std::fs::write(admin.join("gitdir"), "/forged").unwrap();
        assert!(resolve_linked_worktree_source(&linked, &DefaultGitLayoutProbe).is_none());
    }
}
