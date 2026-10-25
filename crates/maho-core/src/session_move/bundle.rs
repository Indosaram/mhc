//! Port of senpi packages/coding-agent/src/core/session-move/bundle.ts.

use std::io::Read;
use std::path::{Component, Path};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const MOVE_BUNDLE_FORMAT: u64 = 1;
pub const MANIFEST_FILE: &str = "manifest.json";
pub const SESSION_BUNDLE_PATH: &str = "files/session.jsonl";

/// senpi `MoveId` (a branded string in `ownership.ts`): a plain id naming one move.
pub type MoveId = String;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BundleFile {
    pub path: String,
    pub sha256: String,
}

/// senpi `RepositoryIdentity` (`repository-identity.ts`): root commits and the normalized origin remote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentity {
    pub root_commits: Vec<String>,
    pub origin_url: Option<String>,
}

pub const EXTENSION_STATE_PREFIX: &str = "state/";
pub const KNOWN_PAYLOAD_KINDS: &[&str] = &["extension-state"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MovePayload {
    pub kind: String,
    pub files: Vec<BundleFile>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveManifest {
    pub format: u64,
    pub move_id: MoveId,
    pub session_id: String,
    pub session_file_name: String,
    pub source_cwd: String,
    pub repository: Option<RepositoryIdentity>,
    pub session: BundleFile,
    pub sidecars: Vec<BundleFile>,
    pub dropped_user_messages: Vec<String>,
    pub payloads: Vec<MovePayload>,
}

/// `MoveBundleInvalidError` (`code: "move_bundle_invalid"`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveBundleInvalidError {
    detail: String,
}

impl MoveBundleInvalidError {
    pub const CODE: &'static str = "move_bundle_invalid";

    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl std::fmt::Display for MoveBundleInvalidError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The moved session could not be verified ({}); it was not imported. The original device keeps it.",
            self.detail
        )
    }
}

impl std::error::Error for MoveBundleInvalidError {}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub async fn sha256_file_async(path: &Path) -> std::io::Result<String> {
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || sha256_file(&path))
        .await
        .map_err(std::io::Error::other)?
}

/// Both separators count: a bundle written on one OS is read on another.
fn leaves_bundle(path: &str) -> bool {
    let bytes = path.as_bytes();
    let windows_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    Path::new(path).is_absolute()
        || path.starts_with('/')
        || path.starts_with('\\')
        || windows_drive
        || path.split(['/', '\\']).any(|segment| segment == "..")
        || Path::new(path)
            .components()
            .any(|component| matches!(component, Component::ParentDir))
}

fn parse_file(value: &Value) -> Result<BundleFile, MoveBundleInvalidError> {
    let (Some(path), Some(sha256)) = (
        value.get("path").and_then(Value::as_str),
        value.get("sha256").and_then(Value::as_str),
    ) else {
        return Err(MoveBundleInvalidError::new("a listed file is malformed"));
    };
    if leaves_bundle(path) {
        return Err(MoveBundleInvalidError::new(format!(
            "a listed path leaves the bundle: {path}"
        )));
    }
    Ok(BundleFile {
        path: path.to_owned(),
        sha256: sha256.to_owned(),
    })
}

/// Move ids and session ids become folder and file names (staging folder, ledger record, resident blobs), so
/// they must be plain ids: a letter or digit first (never `.` or `..`), then letters, digits, `.`, `_`, `-`, at
/// most 128.
pub fn is_plain_id_str(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && value.len() <= 128
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// `isPlainId(value: unknown)`: only a JSON string can be a plain id.
pub fn is_plain_id(value: &Value) -> bool {
    value.as_str().is_some_and(is_plain_id_str)
}

pub fn is_move_id(value: &Value) -> bool {
    is_plain_id(value)
}

pub fn parse_plain_id(value: &Value, what: &str) -> Result<String, MoveBundleInvalidError> {
    match value.as_str() {
        Some(id) if is_plain_id_str(id) => Ok(id.to_owned()),
        _ => Err(MoveBundleInvalidError::new(format!(
            "the {what} is not a plain id: {value}"
        ))),
    }
}

pub fn parse_move_id(value: &Value) -> Result<MoveId, MoveBundleInvalidError> {
    parse_plain_id(value, "move id")
}

/// The importer joins this name onto its session folder, so it must be a plain session file name and nothing else.
fn parse_session_file_name(value: &str) -> Result<String, MoveBundleInvalidError> {
    let stem = value.strip_suffix(".jsonl");
    let plain = stem.is_some_and(|stem| {
        let mut chars = stem.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphanumeric())
            && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    });
    if !plain || value.contains("..") {
        return Err(MoveBundleInvalidError::new(format!(
            "the session file name is not a plain file name: {value}"
        )));
    }
    Ok(value.to_owned())
}

fn is_commit_hash(value: &str) -> bool {
    (40..=64).contains(&value.len()) && value.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

/// senpi `parseRepositoryIdentity`.
pub fn parse_repository_identity(value: &Value) -> Option<RepositoryIdentity> {
    let object = value.as_object()?;
    let roots = object.get("rootCommits")?.as_array()?;
    let mut commits = Vec::new();
    for root in roots {
        commits.push(root.as_str()?.to_owned());
    }
    let origin_url = match object.get("originUrl") {
        None | Some(Value::Null) => None,
        Some(Value::String(url)) => Some(url.clone()),
        Some(_) => return None,
    };
    let mut commits: Vec<String> = commits
        .into_iter()
        .filter(|commit| is_commit_hash(commit))
        .collect();
    commits.sort();
    if commits.is_empty() && origin_url.is_none() {
        return None;
    }
    Some(RepositoryIdentity {
        root_commits: commits,
        origin_url,
    })
}

fn string_field(manifest: &Value, key: &str) -> Result<String, MoveBundleInvalidError> {
    manifest
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| MoveBundleInvalidError::new(format!("missing {key}")))
}

fn parse_payload(value: &Value) -> Result<MovePayload, MoveBundleInvalidError> {
    let kind = value
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| MoveBundleInvalidError::new("a payload is malformed"))?;
    let listed = value
        .get("files")
        .and_then(Value::as_array)
        .ok_or_else(|| MoveBundleInvalidError::new("a payload is malformed"))?;
    if !KNOWN_PAYLOAD_KINDS.contains(&kind) {
        return Err(MoveBundleInvalidError::new(
            "it carries a payload this version cannot import",
        ));
    }
    let files = listed
        .iter()
        .map(parse_file)
        .collect::<Result<Vec<_>, _>>()?;
    for file in &files {
        if !file.path.starts_with(EXTENSION_STATE_PREFIX) {
            return Err(MoveBundleInvalidError::new(format!(
                "an extension-state file is listed outside state/: {}",
                file.path
            )));
        }
    }
    Ok(MovePayload {
        kind: kind.to_owned(),
        files,
    })
}

pub fn read_manifest(bundle_dir: &Path) -> Result<MoveManifest, MoveBundleInvalidError> {
    let raw: Value = std::fs::read_to_string(bundle_dir.join(MANIFEST_FILE))
        .map_err(|error| error.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|error| error.to_string()))
        .map_err(|error| MoveBundleInvalidError::new(format!("manifest unreadable: {error}")))?;
    let format = raw.get("format");
    if format.and_then(Value::as_u64) != Some(MOVE_BUNDLE_FORMAT) {
        let shown = match format {
            None => "undefined".to_owned(),
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
        };
        return Err(MoveBundleInvalidError::new(format!(
            "unknown bundle format {shown}"
        )));
    }
    for key in ["moveId", "sessionId", "sessionFileName", "sourceCwd"] {
        string_field(&raw, key)?;
    }
    let payloads = raw
        .get("payloads")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .map(parse_payload)
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let mut dropped = Vec::new();
    if let Some(messages) = raw.get("droppedUserMessages").and_then(Value::as_array) {
        for message in messages {
            dropped.push(
                message
                    .as_str()
                    .ok_or_else(|| MoveBundleInvalidError::new("malformed queued input"))?
                    .to_owned(),
            );
        }
    }
    let mut sidecars = Vec::new();
    if let Some(listed) = raw.get("sidecars").and_then(Value::as_array) {
        for file in listed {
            sidecars.push(parse_file(file)?);
        }
    }
    Ok(MoveManifest {
        format: MOVE_BUNDLE_FORMAT,
        move_id: parse_move_id(&raw["moveId"])?,
        session_id: parse_plain_id(&raw["sessionId"], "session id")?,
        session_file_name: parse_session_file_name(&string_field(&raw, "sessionFileName")?)?,
        source_cwd: string_field(&raw, "sourceCwd")?,
        repository: raw.get("repository").and_then(parse_repository_identity),
        session: parse_file(raw.get("session").unwrap_or(&Value::Null))?,
        sidecars,
        dropped_user_messages: dropped,
        payloads,
    })
}

pub fn verify_bundle(
    bundle_dir: &Path,
    manifest: &MoveManifest,
) -> Result<(), MoveBundleInvalidError> {
    for file in std::iter::once(&manifest.session)
        .chain(manifest.sidecars.iter())
        .chain(
            manifest
                .payloads
                .iter()
                .flat_map(|payload| payload.files.iter()),
        )
    {
        let actual = sha256_file(&bundle_dir.join(&file.path))
            .map_err(|_| MoveBundleInvalidError::new(format!("missing {}", file.path)))?;
        if actual != file.sha256 {
            return Err(MoveBundleInvalidError::new(format!(
                "{} changed in transit",
                file.path
            )));
        }
    }
    let content =
        std::fs::read_to_string(bundle_dir.join(&manifest.session.path)).unwrap_or_default();
    let header = content.split('\n').next().unwrap_or("");
    let id = serde_json::from_str::<Value>(header)
        .ok()
        .and_then(|value| value.get("id").and_then(Value::as_str).map(str::to_owned));
    if id.as_deref() != Some(manifest.session_id.as_str()) {
        return Err(MoveBundleInvalidError::new(
            "the session file's own id does not match the bundle's session id",
        ));
    }
    Ok(())
}

/// Keep bundle verification off the async executor while streaming large files.
pub async fn verify_bundle_async(
    bundle_dir: &Path,
    manifest: &MoveManifest,
) -> Result<(), MoveBundleInvalidError> {
    let dir = bundle_dir.to_owned();
    let manifest = manifest.clone();
    tokio::task::spawn_blocking(move || verify_bundle(&dir, &manifest))
        .await
        .map_err(|error| MoveBundleInvalidError::new(error.to_string()))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn plain_ids_start_alphanumeric_and_stay_short() {
        assert!(is_plain_id(&json!("abc-1.2_x")));
        assert!(is_plain_id(&json!("a".repeat(128))));
        assert!(!is_plain_id(&json!("a".repeat(129))));
        for bad in ["", ".", "..", ".hidden", "-x", "a/b", "a b"] {
            assert!(!is_plain_id(&json!(bad)), "{bad}");
        }
        assert!(!is_plain_id(&json!(7)));
        assert!(!is_plain_id(&Value::Null));
        assert!(is_move_id(&json!("m1")));
    }

    #[test]
    fn parse_plain_id_names_the_offender() {
        assert_eq!(
            parse_plain_id(&json!("ok"), "session id"),
            Ok("ok".to_owned())
        );
        let error = parse_plain_id(&json!("../x"), "session id").expect_err("rejected");
        assert!(
            error
                .to_string()
                .contains("the session id is not a plain id: \"../x\"")
        );
        let error = parse_move_id(&json!(3)).expect_err("rejected");
        assert!(
            error
                .to_string()
                .contains("the move id is not a plain id: 3")
        );
    }

    #[test]
    fn listed_paths_cannot_leave_the_bundle() {
        for path in ["/etc/passwd", "C:\\x", "..\\x", "a/../b", "\\\\host\\share"] {
            assert!(
                parse_file(&json!({"path": path, "sha256": "x"})).is_err(),
                "{path}"
            );
        }
        assert!(parse_file(&json!({"path": "files/session.jsonl", "sha256": "x"})).is_ok());
    }

    #[test]
    fn manifest_round_trips_and_verifies() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("files")).expect("files");
        let session = "{\"id\":\"s1\"}\n";
        std::fs::write(dir.path().join(SESSION_BUNDLE_PATH), session).expect("session");
        let sha = hex::encode(Sha256::digest(session.as_bytes()));
        let manifest = json!({
            "format": 1, "moveId": "m1", "sessionId": "s1", "sessionFileName": "s1.jsonl", "sourceCwd": "/x",
            "session": {"path": SESSION_BUNDLE_PATH, "sha256": sha}, "sidecars": [], "droppedUserMessages": ["hi"], "payloads": [],
        });
        std::fs::write(dir.path().join(MANIFEST_FILE), manifest.to_string()).expect("manifest");
        let read = read_manifest(dir.path()).expect("manifest");
        assert_eq!(read.move_id, "m1");
        assert_eq!(read.dropped_user_messages, vec!["hi".to_owned()]);
        assert_eq!(verify_bundle(dir.path(), &read), Ok(()));
        std::fs::write(dir.path().join(SESSION_BUNDLE_PATH), "{\"id\":\"s2\"}\n").expect("tamper");
        assert!(
            verify_bundle(dir.path(), &read)
                .expect_err("changed")
                .to_string()
                .contains("changed in transit")
        );
    }

    #[test]
    fn payloads_and_bad_session_file_names_are_refused() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = json!({"format": 1, "moveId": "m1", "sessionId": "s1", "sessionFileName": "s1.jsonl", "sourceCwd": "/x", "session": {"path": "a", "sha256": "b"}});
        let mut with_payload = base.clone();
        with_payload["payloads"] = json!([{"kind": "folder", "files": []}]);
        std::fs::write(dir.path().join(MANIFEST_FILE), with_payload.to_string()).expect("manifest");
        assert!(
            read_manifest(dir.path())
                .expect_err("payload")
                .to_string()
                .contains("payload this version cannot import")
        );
        let mut bad_name = base;
        bad_name["sessionFileName"] = json!("../x.jsonl");
        std::fs::write(dir.path().join(MANIFEST_FILE), bad_name.to_string()).expect("manifest");
        assert!(
            read_manifest(dir.path())
                .expect_err("name")
                .to_string()
                .contains("not a plain file name")
        );
    }
}
