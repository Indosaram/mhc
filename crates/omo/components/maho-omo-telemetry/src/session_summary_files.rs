//! Port of `packages/omo-senpi/src/components/telemetry/session-summary-files.ts`.
//!
//! Every write is a temp file renamed over the target, so a reader (or a process killed mid-write) sees
//! either the previous complete file or the new complete file, never a torn one.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::session_summary_owner::ProcessOwner;

/// The result of a read that must tell an absent file from an unreadable one.
#[derive(Debug, Clone, PartialEq)]
pub enum JsonRead {
    /// No file at the path.
    Missing,
    /// The file exists and is not valid JSON.
    Corrupt,
    Value(Value),
}

impl JsonRead {
    pub fn value(self) -> Option<Value> {
        match self {
            Self::Value(value) => Some(value),
            Self::Missing | Self::Corrupt => None,
        }
    }
}

fn open_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn create_dir_private(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ignored = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

pub fn temp_path(path: &Path, owner: &ProcessOwner) -> PathBuf {
    PathBuf::from(format!(
        "{}.{}-{}.tmp",
        path.display(),
        owner.pid,
        owner.boot_id
    ))
}

/// `writeJsonAtomic`: a private temp file, then a rename over the target.
pub fn write_json_atomic(path: &Path, value: &Value, owner: &ProcessOwner) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        create_dir_private(parent)?;
    }
    let temp = temp_path(path, owner);
    open_private(&temp)?.write_all(value.to_string().as_bytes())?;
    std::fs::rename(&temp, path)
}

/// `readJsonOrCorrupt`: absent and unreadable are distinct answers.
pub fn read_json_or_corrupt(path: &Path) -> JsonRead {
    let Ok(text) = std::fs::read_to_string(path) else {
        return JsonRead::Missing;
    };
    match serde_json::from_str(&text) {
        Ok(value) => JsonRead::Value(value),
        Err(_) => JsonRead::Corrupt,
    }
}

pub fn remove_file(path: &Path) {
    let _ignored = std::fs::remove_file(path);
}

pub fn list_names(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Atomic hand-over: exactly one of several racing renames of the same source succeeds.
pub fn rename_if_present(from: &Path, to: &Path) -> bool {
    std::fs::rename(from, to).is_ok()
}

/// `linkSync`, tolerating an existing destination.
pub fn link_if_absent(from: &Path, to: &Path) -> bool {
    std::fs::hard_link(from, to).is_ok()
}

pub fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-'))
}

pub fn is_safe_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f' | b'-'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn owner() -> ProcessOwner {
        ProcessOwner {
            pid: 4242,
            boot_id: "boot-a".into(),
        }
    }

    #[test]
    fn atomic_write_is_owner_qualified_and_complete() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/window.json");
        write_json_atomic(&path, &json!({"v":2,"seq":3}), &owner()).unwrap();
        assert_eq!(read_json_or_corrupt(&path).value(), Some(json!({"v":2,"seq":3})));
        assert_eq!(list_names(&dir.path().join("nested")), vec!["window.json".to_string()]);
        assert!(temp_path(&path, &owner())
            .file_name()
            .unwrap()
            .to_string_lossy()
            .ends_with(".4242-boot-a.tmp"));
    }

    #[cfg(unix)]
    #[test]
    fn written_file_and_directory_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private/window.json");
        write_json_atomic(&path, &json!({}), &owner()).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(dir.path().join("private"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn absent_and_corrupt_reads_are_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("window.json");
        assert_eq!(read_json_or_corrupt(&path), JsonRead::Missing);
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(read_json_or_corrupt(&path), JsonRead::Corrupt);
        assert_eq!(read_json_or_corrupt(&path).value(), None);
        std::fs::write(&path, "{\"v\":2}").unwrap();
        assert_eq!(read_json_or_corrupt(&path), JsonRead::Value(json!({"v":2})));
    }

    #[test]
    fn rename_hand_over_has_exactly_one_winner() {
        let dir = tempfile::tempdir().unwrap();
        let from = dir.path().join("window.json");
        std::fs::write(&from, "{}").unwrap();
        assert!(rename_if_present(&from, &dir.path().join("first.claimed")));
        assert!(!rename_if_present(&from, &dir.path().join("second.claimed")));
        assert_eq!(list_names(dir.path()), vec!["first.claimed".to_string()]);
    }

    #[test]
    fn safe_names_and_uuids_are_bounded() {
        assert!(is_safe_name("abc:DEF_1-2"));
        assert!(!is_safe_name(""));
        assert!(!is_safe_name("a/b"));
        assert!(!is_safe_name(&"a".repeat(129)));
        // session-summary-storage.ts: SAFE_UUID accepts exactly 36 lowercase hex/hyphen bytes.
        assert!(is_safe_uuid("01234567-89ab-cdef-0123-456789abcdef"));
        assert!(!is_safe_uuid("0123456789ab-cdef-0123-456789abcdef"));
        assert!(!is_safe_uuid("01234567-89AB-cdef-0123-456789abcdef"));
        assert!(!is_safe_uuid("0123456789abcdef"));
        assert!(!is_safe_uuid("z123456789ab-cdef-0123-456789abcdef"));
    }
}
