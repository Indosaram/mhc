//! Port of senpi packages/coding-agent/src/core/session-file-identity.ts.
//!
//! The identity is read from the owning descriptor (`fstat`), never from a later file at its pathname.
//! senpi captures it asynchronously and keys loaded entries to it through a WeakMap; here `fstat` is
//! synchronous, so a loader returns the identity next to its entries instead.

use std::fs::File;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The content fingerprint's own domain (RULING-72 (1)): never a catalog domain (RULING-57 (1)).
pub const BASE_CONTENT_DOMAIN: &str = "senpi-recovery-base/1/sha256";

/// A base's content fingerprint (RULING-72): SHA-256 over the domain tag, NUL, then each counted
/// entry line (UTF-8, as read; skipped blank and malformed lines excluded) and LF.
#[derive(Clone, Debug)]
pub struct BaseContentHash {
    hash: Sha256,
}

impl Default for BaseContentHash {
    fn default() -> Self {
        Self::new()
    }
}

impl BaseContentHash {
    pub fn new() -> Self {
        let mut hash = Sha256::new();
        hash.update(BASE_CONTENT_DOMAIN.as_bytes());
        hash.update(b"\0");
        Self { hash }
    }

    pub fn line(&mut self, text: &str) {
        self.hash.update(text.as_bytes());
        self.hash.update(b"\n");
    }

    pub fn digest(self) -> String {
        hex::encode(self.hash.finalize())
    }
}

/// The file a persisted base was read from. A rewrite through rename, or any other file at that path, differs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileIdentity {
    pub path: String,
    pub dev: String,
    pub ino: String,
}

/// Capture the owning descriptor's identity; `None` when the platform or the descriptor cannot say.
pub fn capture_file_identity(file: &File, path: &str) -> Option<FileIdentity> {
    identity_of(file, path)
}

/// Capture the identity, then close the descriptor (senpi `captureAndCloseFileIdentity`).
pub fn capture_and_close_file_identity(file: File, path: &str) -> Option<FileIdentity> {
    let identity = capture_file_identity(&file, path);
    drop(file);
    identity
}

#[cfg(unix)]
fn identity_of(file: &File, path: &str) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().ok()?;
    Some(FileIdentity {
        path: path.to_owned(),
        dev: metadata.dev().to_string(),
        ino: metadata.ino().to_string(),
    })
}

/// Windows reports the same identity through `GetFileInformationByHandle`: the volume serial number
/// plus the file index. `std::fs::Metadata` does not expose them on stable, so the descriptor is
/// queried directly; the identity must stay tied to the handle this process opened.
#[cfg(windows)]
#[allow(unsafe_code)]
fn identity_of(file: &File, path: &str) -> Option<FileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };

    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: `file` owns a live handle for the duration of the call, and `info` is a valid
    // out-pointer to one BY_HANDLE_FILE_INFORMATION.
    let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
    if ok == 0 {
        return None;
    }
    let index = (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow);
    Some(FileIdentity {
        path: path.to_owned(),
        dev: info.dwVolumeSerialNumber.to_string(),
        ino: index.to_string(),
    })
}

#[cfg(not(any(unix, windows)))]
fn identity_of(_file: &File, _path: &str) -> Option<FileIdentity> {
    None
}

/// Whether the descriptor is still the file `identity` names.
pub fn same_file(file: &File, identity: &FileIdentity) -> bool {
    identity_of(file, &identity.path)
        .is_some_and(|current| current.dev == identity.dev && current.ino == identity.ino)
}

#[cfg(all(test, any(unix, windows)))]
mod tests {
    use super::*;

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
        assert!(!same_file(&reopened, &first));
        let second = capture_file_identity(&reopened, &path_text).expect("identity");
        assert!(same_file(&reopened, &second));
    }
}
