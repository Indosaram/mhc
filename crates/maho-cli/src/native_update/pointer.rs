use std::io::Write;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "current.json";
const SCHEMA_VERSION: u32 = 1;

/// `R/current.json`: which `versions/<v>` new launches use.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pointer {
    pub schema_version: u32,
    pub current: String,
    pub previous: Option<String>,
}

impl Pointer {
    pub fn new(current: String, previous: Option<String>) -> Self {
        Self { schema_version: SCHEMA_VERSION, current, previous }
    }
}

pub fn is_valid_version(version: &str) -> bool {
    !version.is_empty()
        && version.len() <= 128
        && !version.starts_with('.')
        && version.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
}

/// Directory name for a release version: the string `mhc --version` prints, without a leading `v`.
pub fn version_dir(version: &str) -> Result<String, String> {
    let version = version.trim();
    let version = version.strip_prefix(['v', 'V']).unwrap_or(version);
    if is_valid_version(version) {
        Ok(version.to_owned())
    } else {
        Err(format!("unsupported release version: {version:?}"))
    }
}

pub fn read(root: &Path) -> Result<Pointer, String> {
    let path = root.join(FILE_NAME);
    let bytes = std::fs::read(&path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let pointer: Pointer = serde_json::from_slice(&bytes)
        .map_err(|error| format!("invalid {}: {error}", path.display()))?;
    if pointer.schema_version != SCHEMA_VERSION
        || !is_valid_version(&pointer.current)
        || pointer.previous.as_deref().is_some_and(|previous| !is_valid_version(previous))
    {
        return Err(format!("unsupported {}", path.display()));
    }
    Ok(pointer)
}

/// Atomically replaces `R/current.json`: temp file in `R`, fsync, rename.
pub fn publish(root: &Path, pointer: &Pointer) -> Result<(), String> {
    let mut temporary = tempfile::Builder::new()
        .prefix(".current.json.")
        .tempfile_in(root)
        .map_err(|error| format!("cannot create pointer file: {error}"))?;
    serde_json::to_writer(&mut temporary, pointer).map_err(|error| error.to_string())?;
    temporary.write_all(b"\n").map_err(|error| error.to_string())?;
    temporary.as_file().sync_all().map_err(|error| error.to_string())?;
    temporary
        .persist(root.join(FILE_NAME))
        .map_err(|error| format!("cannot publish {FILE_NAME}: {}", error.error))?;
    #[cfg(unix)]
    std::fs::File::open(root)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| format!("cannot sync {}: {error}", root.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn publish_replaces_pointer_atomically_and_leaves_no_temporary_files() -> TestResult {
        let root = tempfile::tempdir()?;
        publish(root.path(), &Pointer::new("1.0.0".into(), None))?;
        let next = Pointer::new("1.1.0".into(), Some("1.0.0".into()));
        publish(root.path(), &next)?;
        assert_eq!(read(root.path())?, next);
        assert_eq!(std::fs::read_dir(root.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn parses_documented_shape() -> TestResult {
        let root = tempfile::tempdir()?;
        std::fs::write(
            root.path().join(FILE_NAME),
            br#"{"schemaVersion":1,"current":"1.2.3","previous":null}"#,
        )?;
        assert_eq!(read(root.path())?, Pointer::new("1.2.3".into(), None));
        Ok(())
    }

    #[test]
    fn rejects_unknown_schema_and_unsafe_versions() -> TestResult {
        let root = tempfile::tempdir()?;
        for body in [
            r#"{"schemaVersion":2,"current":"1.0.0","previous":null}"#,
            r#"{"schemaVersion":1,"current":"../evil","previous":null}"#,
            r#"{"schemaVersion":1,"current":"1.0.0","previous":"a/b"}"#,
            "not json",
        ] {
            std::fs::write(root.path().join(FILE_NAME), body)?;
            assert!(read(root.path()).is_err(), "{body}");
        }
        assert!(read(&root.path().join("missing")).is_err());
        Ok(())
    }

    #[test]
    fn version_dir_strips_leading_v_and_rejects_paths() {
        assert_eq!(version_dir("v1.2.3"), Ok("1.2.3".to_owned()));
        assert_eq!(version_dir("1.2.3-rc.1+build"), Ok("1.2.3-rc.1+build".to_owned()));
        for bad in ["", "..", "a/b", "a\\b", ".hidden"] {
            assert!(version_dir(bad).is_err(), "{bad:?}");
        }
    }
}
