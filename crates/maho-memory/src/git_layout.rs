//! Finding the main checkout behind a linked git worktree, without running git.
//!
//! Spec: `docs/cleanroom/specs/memory-linked-worktrees.md`. A session may run in a linked worktree while memory is
//! keyed by the main checkout, so identity has to know which repository a directory really belongs to. Shelling
//! out to git on every turn is too slow and fails when git is absent, so this reads the on-disk layout directly.

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkedWorktreeSource {
    pub hash_source: PathBuf,
    pub slug_source: String,
}

/// Resolve `path` against `base` and collapse `.`/`..` lexically, as `path.resolve` does.
pub fn resolve_from(base: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    };
    normalize(&joined)
}

/// Collapse `.` and `..` without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            std::path::Component::RootDir => out.push(std::path::Component::RootDir.as_os_str()),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            std::path::Component::Normal(segment) => out.push(segment),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

fn name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn first_line(text: Option<String>) -> Option<String> {
    text?.lines().next().map(|line| line.trim().to_owned())
}

/// Walk up to the nearest `.git` marker.
///
/// A **directory** marker is an ordinary checkout and stops the search with `None`: a nested repository inside a
/// repository belongs to itself, not to an outer worktree.
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
    // Without the back-pointer this is just a file that looks like a pointer; a forged or stale marker must not
    // redirect memory to an unrelated repository.
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
        let directory = name(common);
        let slug = directory
            .strip_suffix(".git")
            .filter(|s| !s.is_empty())
            .unwrap_or(&directory)
            .to_owned();
        return Some((common.to_path_buf(), slug));
    }
    None
}

/// Minimal `[core]` reader: only `bare` and `worktree` matter, and only inside the core section.
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
        // Git allows a trailing comment with either marker; a `#` inside a path is not a comment without the
        // leading space, so only ` #` and ` ;` are cut.
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
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeProbe {
        kinds: BTreeMap<PathBuf, GitEntryKind>,
        texts: BTreeMap<PathBuf, String>,
        reals: BTreeMap<PathBuf, PathBuf>,
    }

    impl FakeProbe {
        fn with_kind(mut self, path: &str, kind: GitEntryKind) -> Self {
            self.kinds.insert(normalize(Path::new(path)), kind);
            self
        }

        fn with_text(mut self, path: &str, text: &str) -> Self {
            self.texts
                .insert(normalize(Path::new(path)), text.to_owned());
            self
        }

        fn with_real(mut self, path: &str, real: &str) -> Self {
            self.reals
                .insert(normalize(Path::new(path)), normalize(Path::new(real)));
            self
        }
    }

    impl GitLayoutProbe for FakeProbe {
        fn entry_kind(&self, path: &Path) -> Option<GitEntryKind> {
            self.kinds.get(&normalize(path)).copied()
        }

        fn read_text(&self, path: &Path) -> Option<String> {
            self.texts.get(&normalize(path)).cloned()
        }

        fn real_path(&self, path: &Path) -> Option<PathBuf> {
            self.reals.get(&normalize(path)).cloned()
        }
    }

    const LINKED: &str = "/repo/linked";
    const ADMIN: &str = "/repo/main/.git/worktrees/linked";

    fn linked_worktree_probe() -> FakeProbe {
        FakeProbe::default()
            .with_kind("/repo/linked/.git", GitEntryKind::File)
            .with_text("/repo/linked/.git", &format!("gitdir: {ADMIN}\n"))
            .with_text(&format!("{ADMIN}/commondir"), "../..\n")
            .with_text(&format!("{ADMIN}/gitdir"), "/repo/linked/.git")
            .with_real("/repo/linked/.git", LINKED)
            .with_real(ADMIN, ADMIN)
    }

    #[test]
    fn a_subdirectory_resolves_to_the_main_checkout_and_keeps_the_subpath() {
        let source =
            resolve_linked_worktree_source(Path::new("/repo/linked/src"), &linked_worktree_probe())
                .expect("a linked worktree should resolve");
        assert_eq!(source.hash_source, Path::new("/repo/main/src"));
        assert_eq!(source.slug_source, "src");
    }

    #[test]
    fn the_worktree_root_uses_the_main_directory_name_as_slug() {
        let source =
            resolve_linked_worktree_source(Path::new(LINKED), &linked_worktree_probe()).unwrap();
        assert_eq!(source.hash_source, Path::new("/repo/main"));
        assert_eq!(source.slug_source, "main");
    }

    #[test]
    fn an_ordinary_checkout_is_not_a_linked_worktree() {
        let probe = FakeProbe::default().with_kind("/repo/.git", GitEntryKind::Directory);
        assert!(resolve_linked_worktree_source(Path::new("/repo/src"), &probe).is_none());
    }

    #[test]
    fn a_marker_without_the_gitdir_prefix_is_rejected() {
        let mut probe = linked_worktree_probe();
        probe
            .texts
            .insert(PathBuf::from("/repo/linked/.git"), "not a pointer\n".into());
        assert!(resolve_linked_worktree_source(Path::new(LINKED), &probe).is_none());
    }

    #[test]
    fn an_empty_target_is_rejected() {
        let mut probe = linked_worktree_probe();
        probe
            .texts
            .insert(PathBuf::from("/repo/linked/.git"), "gitdir:\n".into());
        assert!(resolve_linked_worktree_source(Path::new(LINKED), &probe).is_none());
    }

    #[test]
    fn a_missing_commondir_is_rejected() {
        let mut probe = linked_worktree_probe();
        probe.texts.remove(Path::new(&format!("{ADMIN}/commondir")));
        assert!(resolve_linked_worktree_source(Path::new(LINKED), &probe).is_none());
    }

    #[test]
    fn a_forged_back_pointer_is_rejected() {
        let mut probe = linked_worktree_probe();
        probe
            .texts
            .insert(PathBuf::from(format!("{ADMIN}/gitdir")), "/forged".into());
        assert!(resolve_linked_worktree_source(Path::new(LINKED), &probe).is_none());
    }

    #[test]
    fn a_missing_back_pointer_is_rejected() {
        let mut probe = linked_worktree_probe();
        probe.texts.remove(Path::new(&format!("{ADMIN}/gitdir")));
        assert!(resolve_linked_worktree_source(Path::new(LINKED), &probe).is_none());
    }

    #[test]
    fn a_crlf_pointer_file_is_accepted() {
        let mut probe = linked_worktree_probe();
        probe.texts.insert(
            PathBuf::from("/repo/linked/.git"),
            format!("gitdir: {ADMIN}\r\n"),
        );
        assert!(resolve_linked_worktree_source(Path::new(LINKED), &probe).is_some());
    }

    #[test]
    fn a_bare_repository_uses_its_own_directory() {
        let common = "/repo/main/bare.git";
        let probe = FakeProbe::default()
            .with_kind("/repo/linked/.git", GitEntryKind::File)
            .with_text("/repo/linked/.git", &format!("gitdir: {ADMIN}\n"))
            .with_text(&format!("{ADMIN}/commondir"), "../../../bare.git\n")
            .with_text(&format!("{ADMIN}/gitdir"), "/repo/linked/.git")
            .with_real("/repo/linked/.git", LINKED)
            .with_real(ADMIN, ADMIN)
            .with_text(&format!("{common}/config"), "[core]\n\tbare = true\n");
        let source = resolve_linked_worktree_source(Path::new(LINKED), &probe).unwrap();
        assert_eq!(source.hash_source, Path::new(common));
        assert_eq!(source.slug_source, "bare");
    }

    #[test]
    fn a_core_worktree_setting_beats_the_bare_layout() {
        let common = "/repo/main/admin";
        let probe = FakeProbe::default()
            .with_kind("/repo/linked/.git", GitEntryKind::File)
            .with_text("/repo/linked/.git", &format!("gitdir: {ADMIN}\n"))
            .with_text(&format!("{ADMIN}/commondir"), "../../../admin\n")
            .with_text(&format!("{ADMIN}/gitdir"), "/repo/linked/.git")
            .with_real("/repo/linked/.git", LINKED)
            .with_real(ADMIN, ADMIN)
            .with_text(
                &format!("{common}/config"),
                "[core]\n\tbare = false\n\tworktree = /somewhere/checked\n",
            );
        let source = resolve_linked_worktree_source(Path::new(LINKED), &probe).unwrap();
        assert_eq!(source.hash_source, Path::new("/somewhere/checked"));
        assert_eq!(source.slug_source, "checked");
    }

    #[test]
    fn a_repository_with_neither_bare_nor_worktree_is_rejected() {
        let probe = FakeProbe::default()
            .with_kind("/repo/linked/.git", GitEntryKind::File)
            .with_text("/repo/linked/.git", &format!("gitdir: {ADMIN}\n"))
            .with_text(&format!("{ADMIN}/commondir"), "../../../admin\n")
            .with_text(&format!("{ADMIN}/gitdir"), "/repo/linked/.git")
            .with_real("/repo/linked/.git", LINKED)
            .with_real(ADMIN, ADMIN)
            .with_text("/repo/main/admin/config", "[core]\n\tbare = false\n");
        assert!(resolve_linked_worktree_source(Path::new(LINKED), &probe).is_none());
    }

    #[test]
    fn core_config_reads_bare_forms() {
        for text in ["true", "yes", "on", "1", "TRUE"] {
            assert_eq!(
                read_core_config(&format!("[core]\nbare = {text}\n")).0,
                Some(true),
                "{text}"
            );
        }
        assert_eq!(read_core_config("[core]\nbare = false\n").0, Some(false));
    }

    #[test]
    fn core_config_ignores_keys_outside_the_core_section() {
        let (bare, worktree) = read_core_config("[user]\nbare = true\nworktree = /nope\n");
        assert_eq!(bare, None);
        assert_eq!(worktree, None);
    }

    #[test]
    fn core_config_stops_reading_core_at_the_next_section() {
        let (bare, _) = read_core_config("[core]\nbare = true\n[remote]\nbare = false\n");
        assert_eq!(bare, Some(true));
    }

    #[test]
    fn core_config_trims_quotes_and_trailing_comments() {
        let (_, worktree) = read_core_config("[core]\nworktree = \"/some path\" # a comment\n");
        assert_eq!(worktree.as_deref(), Some("/some path"));
    }

    #[test]
    fn core_config_keeps_a_hash_that_is_not_a_comment() {
        let (_, worktree) = read_core_config("[core]\nworktree = /a#b\n");
        assert_eq!(worktree.as_deref(), Some("/a#b"));
    }

    #[test]
    fn an_unreadable_config_yields_nothing() {
        assert_eq!(read_core_config(""), (None, None));
    }
}
