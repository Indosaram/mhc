//! Port of senpi packages/coding-agent/src/core/session-file-identity.ts.
//!
//! The identity is read from the owning descriptor (`fstat`), never from a later file at its pathname.
//! senpi captures it asynchronously and keys loaded entries to it through a WeakMap; here `fstat` is
//! synchronous, so a loader returns the identity next to its entries instead.

use std::fs::File;

use serde::{Deserialize, Serialize};

/// The file a persisted base was read from. A rewrite through rename, or any other file at that path, differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub path: String,
    pub dev: String,
    pub ino: String,
}

/// Capture the owning descriptor's identity; `None` when the platform or the descriptor cannot say.
pub fn capture_file_identity(file: &File, path: &str) -> Option<FileIdentity> {
    let metadata = file.metadata().ok()?;
    identity_of(&metadata, path)
}

/// Capture the identity, then close the descriptor (senpi `captureAndCloseFileIdentity`).
pub fn capture_and_close_file_identity(file: File, path: &str) -> Option<FileIdentity> {
    let identity = capture_file_identity(&file, path);
    drop(file);
    identity
}

#[cfg(unix)]
fn identity_of(metadata: &std::fs::Metadata, path: &str) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    Some(FileIdentity {
        path: path.to_owned(),
        dev: metadata.dev().to_string(),
        ino: metadata.ino().to_string(),
    })
}

/// Stable Rust exposes no file index on Windows; the capture reports unavailable, as a failed fstat does upstream.
#[cfg(not(unix))]
fn identity_of(_metadata: &std::fs::Metadata, _path: &str) -> Option<FileIdentity> {
    None
}

/// Whether `metadata` (of an open descriptor) is the file `identity` names.
pub fn same_file(metadata: &std::fs::Metadata, identity: &FileIdentity) -> bool {
    identity_of(metadata, &identity.path)
        .is_some_and(|current| current.dev == identity.dev && current.ino == identity.ino)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_rename_over_the_path_is_a_different_identity() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("s.jsonl");
        std::fs::write(&path, "a\n").expect("write");
        let path_text = path.to_string_lossy().into_owned();
        let first = capture_and_close_file_identity(File::open(&path).expect("open"), &path_text)
            .expect("identity");
        assert_eq!(first.path, path_text);
        let replacement = dir.path().join("next");
        std::fs::write(&replacement, "a\n").expect("write");
        std::fs::rename(&replacement, &path).expect("rename");
        let reopened = File::open(&path).expect("open");
        assert!(!same_file(&reopened.metadata().expect("stat"), &first));
        let second = capture_file_identity(&reopened, &path_text).expect("identity");
        assert!(same_file(&reopened.metadata().expect("stat"), &second));
    }
}
