//! The key a session file is reserved under (senpi `rpc/session-path-key.ts`): one key for every spelling
//! of one file. A path whose directory is gone keeps its missing tail verbatim under the deepest ancestor
//! that still exists, so another session's deleted directory can never fail a listing or an open
//! (senpi#2206).
use std::path::{Path, PathBuf};

fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    // Lexical normalization, as node's `path.resolve` does.
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

pub fn canonical_session_path(path: &Path) -> PathBuf {
    let absolute_path = absolute(path);
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut existing = absolute_path.clone();
    while !existing.exists() {
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return absolute_path;
        };
        missing.insert(0, name.to_owned());
        existing = parent.to_path_buf();
    }
    // node `realpathSync` never spells a Windows path with the `\\?\` verbatim prefix.
    match std::fs::canonicalize(&existing).map(maho_core::paths::strip_verbatim_prefix) {
        Ok(mut real) => {
            real.extend(missing);
            real
        }
        // A delete racing the existence check leaves the given spelling as the only one.
        Err(_) => absolute_path,
    }
}

/// The directory a session writes its transcript into is gone: it can never persist again.
pub fn session_directory_removed(session_file: &Path) -> bool {
    match absolute(session_file).parent() {
        Some(parent) => !parent.exists(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_tail_is_kept_under_the_real_ancestor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = maho_core::paths::strip_verbatim_prefix(
            std::fs::canonicalize(dir.path()).expect("canonical"),
        );
        let path = dir.path().join("gone").join("s.jsonl");
        assert_eq!(
            canonical_session_path(&path),
            real.join("gone").join("s.jsonl")
        );
        assert!(session_directory_removed(&path));
        assert!(!session_directory_removed(&dir.path().join("s.jsonl")));
    }

    #[cfg(unix)]
    #[test]
    fn every_spelling_of_one_file_has_one_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target");
        std::fs::create_dir(&target).expect("mkdir");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        assert_eq!(
            canonical_session_path(&link.join("s.jsonl")),
            canonical_session_path(&target.join("..").join("target").join("s.jsonl"))
        );
    }
}
