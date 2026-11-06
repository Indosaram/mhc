//! Port of senpi packages/coding-agent/src/core/session-cwd.ts.

use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCwdIssue {
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

/// The slice of SessionManager this module reads.
pub trait SessionCwdSource {
    fn get_cwd(&self) -> &str;
    fn get_session_file(&self) -> Option<&str>;
}

impl SessionCwdSource for crate::session_manager::SessionManager {
    fn get_cwd(&self) -> &str {
        self.cwd()
    }
    fn get_session_file(&self) -> Option<&str> {
        self.session_file()
    }
}

pub fn get_missing_session_cwd_issue(
    session_manager: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    let session_file = session_manager.get_session_file()?;
    let session_cwd = session_manager.get_cwd();
    if session_cwd.is_empty() || Path::new(session_cwd).exists() {
        return None;
    }
    Some(SessionCwdIssue {
        session_file: Some(session_file.to_owned()),
        session_cwd: session_cwd.to_owned(),
        fallback_cwd: fallback_cwd.to_owned(),
    })
}

pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = issue
        .session_file
        .as_ref()
        .map(|file| format!("\nSession file: {file}"))
        .unwrap_or_default();
    format!(
        "Stored session working directory does not exist: {}{session_file}\nCurrent working directory: {}",
        issue.session_cwd, issue.fallback_cwd
    )
}

pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSessionCwdError {
    pub issue: SessionCwdIssue,
}

impl fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_missing_session_cwd_error(&self.issue))
    }
}

impl std::error::Error for MissingSessionCwdError {}

pub fn assert_session_cwd_exists(
    session_manager: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(session_manager, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError { issue }),
        None => Ok(()),
    }
}

// ---------------------------------------------------------------------------
// Asynchronous session rebind I/O (senpi packages/coding-agent/src/core/session-rebind.ts)
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SessionHeaderLine {
    #[serde(rename = "type")]
    pub entry_type: String,
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

pub async fn read_session_cwd(
    session_file: impl AsRef<Path>,
) -> Result<Option<String>, std::io::Error> {
    let header = read_header(session_file.as_ref()).await?;
    Ok(header.cwd.filter(|s| !s.is_empty()))
}

/// senpi `readRecordedRepositoryIdentity`: the newest valid `repository-identity` entry, read asynchronously.
pub async fn read_recorded_repository_identity(
    session_file: &Path,
) -> Option<crate::session_move::bundle::RepositoryIdentity> {
    let mut recorded = None;
    for entry in crate::session_entry_file::load_entries_from_file_async(session_file, None).await {
        if entry.get("type").and_then(serde_json::Value::as_str) == Some("custom")
            && entry.get("customType").and_then(serde_json::Value::as_str)
                == Some("repository-identity")
        {
            recorded = entry
                .get("data")
                .and_then(crate::session_move::bundle::parse_repository_identity)
                .or(recorded);
        }
    }
    recorded
}

async fn path_exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}

fn parse_header(
    first_line: &str,
    session_file: &Path,
) -> Result<SessionHeaderLine, std::io::Error> {
    let header: SessionHeaderLine = serde_json::from_str(first_line).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Cannot rebind session: {error}"),
        )
    })?;
    if header.entry_type != "session" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "Cannot rebind session: {} does not start with a session header",
                session_file.display()
            ),
        ));
    }
    Ok(header)
}

async fn read_header_line(
    session_file: &Path,
) -> Result<(SessionHeaderLine, String), std::io::Error> {
    let content = tokio::fs::read_to_string(session_file).await?;
    let (first_line, rest) = match content.find('\n') {
        Some(pos) => (&content[..pos], content[pos + 1..].to_string()),
        None => (content.as_str(), String::new()),
    };
    Ok((parse_header(first_line, session_file)?, rest))
}

const HEADER_CHUNK_BYTES: usize = 64 * 1024;

/// The header alone: reads up to the first newline, never the transcript after it.
async fn read_header(session_file: &Path) -> Result<SessionHeaderLine, std::io::Error> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(session_file).await?;
    let mut line = Vec::new();
    let mut buffer = vec![0u8; HEADER_CHUNK_BYTES];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        match chunk.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                line.extend_from_slice(&chunk[..newline]);
                break;
            }
            None => line.extend_from_slice(chunk),
        }
    }
    parse_header(&String::from_utf8_lossy(&line), session_file)
}

// The header read happens before the move lock: a missing source next to an existing target means the move
// is already done (`None`).
async fn read_header_before_lock(
    source: &Path,
    target: &Path,
) -> Result<Option<SessionHeaderLine>, std::io::Error> {
    match read_header(source).await {
        Ok(header) => Ok(Some(header)),
        Err(err)
            if err.kind() == std::io::ErrorKind::NotFound
                && target != source
                && path_exists(target).await =>
        {
            Ok(None)
        }
        Err(err) => Err(err),
    }
}

/// A concurrent rebind writes the target before it removes the source: no source next to a target means done.
async fn moved_already(source: &Path, target: &Path) -> bool {
    !path_exists(source).await && path_exists(target).await
}

async fn move_session_file(
    source: &Path,
    target: &Path,
    cwd: &str,
) -> Result<std::path::PathBuf, std::io::Error> {
    if target != source && moved_already(source, target).await {
        return Ok(target.to_path_buf());
    }
    let (mut header, rest) = read_header_line(source).await?;
    if target != source && path_exists(target).await {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("Cannot rebind session: {} already exists", target.display()),
        ));
    }
    let target_dir = target.parent().unwrap_or_else(|| Path::new("."));
    tokio::fs::create_dir_all(target_dir).await?;

    let filename = source.file_name().unwrap_or_default().to_string_lossy();
    let temp_name = format!(".{filename}.rebind-{}.tmp", std::process::id());
    let temp_path = target_dir.join(temp_name);

    header.cwd = Some(cwd.to_owned());
    let mut serialized = serde_json::to_string(&header)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    serialized.push('\n');
    serialized.push_str(&rest);

    tokio::fs::write(&temp_path, serialized.as_bytes()).await?;
    if target != source {
        move_session_sidecars(
            source.parent().unwrap_or_else(|| Path::new(".")),
            target_dir,
            &header.id,
        )
        .await?;
    }
    tokio::fs::rename(&temp_path, target).await?;
    if target != source {
        match tokio::fs::remove_file(source).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(target.to_path_buf())
}

// Per-session extension state lives at `<sessionDir>/extensions/<extension>/<encoded id>.json`
// (goal, loop, terminal monitors); without it a rebound session would lose its goal and monitors.
async fn move_session_sidecars(
    from_dir: &Path,
    to_dir: &Path,
    session_id: &str,
) -> Result<(), std::io::Error> {
    let extensions_dir = from_dir.join("extensions");
    let sidecar_name = format!(
        "{}.json",
        crate::session_sidecar_store::encoded_session_id(session_id)
    );
    if path_exists(&extensions_dir).await {
        let mut entries = tokio::fs::read_dir(&extensions_dir).await?;
        while let Some(extension) = entries.next_entry().await? {
            if !extension.file_type().await?.is_dir() {
                continue;
            }
            let from = extension.path().join(&sidecar_name);
            if path_exists(&from).await {
                move_path(
                    &from,
                    &to_dir
                        .join("extensions")
                        .join(extension.file_name())
                        .join(&sidecar_name),
                )
                .await?;
            }
        }
    }
    let blobs = from_dir.join("resident-blobs").join(session_id);
    if path_exists(&blobs).await {
        move_path(&blobs, &to_dir.join("resident-blobs").join(session_id)).await?;
    }
    Ok(())
}

async fn move_path(from: &Path, to: &Path) -> Result<(), std::io::Error> {
    if path_exists(to).await {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    match tokio::fs::rename(from, to).await {
        Ok(()) => Ok(()),
        // EXDEV: a rename across devices; copy, then remove the source.
        Err(error) if error.raw_os_error() == Some(18) => {
            copy_tree(from, to).await?;
            if tokio::fs::metadata(from).await?.is_dir() {
                tokio::fs::remove_dir_all(from).await
            } else {
                tokio::fs::remove_file(from).await
            }
        }
        Err(error) => Err(error),
    }
}

fn copy_tree<'a>(
    from: &'a Path,
    to: &'a Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), std::io::Error>> + Send + 'a>> {
    Box::pin(async move {
        if !tokio::fs::metadata(from).await?.is_dir() {
            tokio::fs::copy(from, to).await?;
            return Ok(());
        }
        tokio::fs::create_dir_all(to).await?;
        let mut entries = tokio::fs::read_dir(from).await?;
        while let Some(entry) = entries.next_entry().await? {
            copy_tree(&entry.path(), &to.join(entry.file_name())).await?;
        }
        Ok(())
    })
}

pub async fn rebind_session_file(
    source_path: &Path,
    target_cwd: &Path,
    session_dir: Option<&Path>,
    _move_lock_wait_ms: Option<u64>,
) -> Result<std::path::PathBuf, std::io::Error> {
    let source = tokio::fs::canonicalize(source_path)
        .await
        .unwrap_or_else(|_| source_path.to_path_buf());
    let cwd = tokio::fs::canonicalize(target_cwd)
        .await
        .unwrap_or_else(|_| target_cwd.to_path_buf());
    let default_dir = crate::session_manager::get_default_session_dir(&cwd.to_string_lossy());
    let target_dir = session_dir
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(default_dir));

    let filename = source.file_name().unwrap_or_default();
    let target = target_dir.join(filename);

    if target != source && moved_already(&source, &target).await {
        return Ok(target);
    }

    let Some(header) = read_header_before_lock(&source, &target).await? else {
        return Ok(target);
    };

    // senpi `withSessionMoveLock`: a session another live process holds is refused with
    // `SessionHeldError` naming it, instead of being moved out from under it. The holders are read
    // under the move lock, so one that appeared just before it is still named.
    let held = |holders: Vec<crate::session_holder_records::SessionHolder>| {
        std::io::Error::other(crate::session_hold_errors::SessionHeldError::new(
            source.to_string_lossy(),
            holders,
        ))
    };
    let moved = crate::session_move::session_lock::with_session_move_lock(
        &source,
        async {
            let holders = match live_holders_of(&source, &header.id).await {
                Ok(holders) => holders,
                Err(error) => return Ok(Err(error)),
            };
            if !holders.is_empty() {
                return Ok(Err(held(holders)));
            }
            Ok(move_session_file(&source, &target, &cwd.to_string_lossy()).await)
        },
        None,
    )
    .await;
    match moved {
        Ok(result) => result,
        // The lock itself refused because a live holder or another mover has the session: name the holders
        // when there are any, as the typed check inside the lock would have.
        Err(lock_error) => {
            let holders = live_holders_of(&source, &header.id).await?;
            if holders.is_empty() {
                Err(std::io::Error::other(lock_error))
            } else {
                Err(held(holders))
            }
        }
    }
}

async fn live_holders_of(
    session_file: &Path,
    session_id: &str,
) -> Result<Vec<crate::session_holder_records::SessionHolder>, std::io::Error> {
    let now: crate::schedule::native_probes::NowFn = std::sync::Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    });
    let probes = crate::schedule::native_probes::NativeProbes::new(now)
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    crate::session_holder_records::live_session_holders(session_file, session_id, &probes)
        .await
        .map_err(|error| std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source {
        cwd: String,
        session_file: Option<String>,
    }

    impl SessionCwdSource for Source {
        fn get_cwd(&self) -> &str {
            &self.cwd
        }
        fn get_session_file(&self) -> Option<&str> {
            self.session_file.as_deref()
        }
    }

    #[test]
    fn an_existing_cwd_is_not_an_issue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = Source {
            cwd: dir.path().to_string_lossy().into_owned(),
            session_file: Some("/s.jsonl".into()),
        };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());
    }

    #[test]
    fn a_missing_session_file_is_not_an_issue() {
        let source = Source {
            cwd: "/definitely/missing".into(),
            session_file: None,
        };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());
    }

    #[test]
    fn a_missing_cwd_is_reported_with_the_fallback() {
        let source = Source {
            cwd: "/definitely/missing".into(),
            session_file: Some("/s.jsonl".into()),
        };
        let issue = get_missing_session_cwd_issue(&source, "/fallback").expect("issue");
        assert_eq!(issue.session_cwd, "/definitely/missing");
        assert_eq!(issue.fallback_cwd, "/fallback");
        assert!(format_missing_session_cwd_error(&issue).contains("Session file: /s.jsonl"));
        assert!(format_missing_session_cwd_prompt(&issue).contains("continue in current cwd"));
    }

    #[test]
    fn assert_returns_the_typed_error() {
        let source = Source {
            cwd: "/definitely/missing".into(),
            session_file: Some("/s.jsonl".into()),
        };
        assert!(assert_session_cwd_exists(&source, "/fallback").is_err());
    }

    #[tokio::test]
    async fn read_session_cwd_reads_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session_file = dir.path().join("session.jsonl");
        tokio::fs::write(&session_file, b"{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/my/project\"}\n{\"type\":\"message\"}\n").await.expect("write");
        let cwd = read_session_cwd(&session_file)
            .await
            .expect("read")
            .expect("cwd");
        assert_eq!(cwd, "/my/project");
    }

    #[tokio::test]
    async fn the_header_read_stops_at_the_first_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session_file = dir.path().join("session.jsonl");
        // A second line that is not valid UTF-8 JSON would fail a whole-file read_to_string.
        let mut bytes = b"{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/p\"}\n".to_vec();
        bytes.extend_from_slice(&[0xff; 200_000]);
        tokio::fs::write(&session_file, &bytes)
            .await
            .expect("write");
        assert_eq!(
            read_session_cwd(&session_file)
                .await
                .expect("read")
                .as_deref(),
            Some("/p")
        );
    }

    #[tokio::test]
    async fn a_move_another_rebind_already_finished_returns_its_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = dir.path().join("old/s.jsonl");
        let target = dir.path().join("new/s.jsonl");
        tokio::fs::create_dir_all(target.parent().expect("parent"))
            .await
            .expect("dir");
        tokio::fs::write(&target, b"{\"type\":\"session\",\"id\":\"s1\"}\n")
            .await
            .expect("write");
        assert!(
            read_header_before_lock(&source, &target)
                .await
                .expect("read")
                .is_none()
        );
        assert_eq!(
            move_session_file(&source, &target, "/new")
                .await
                .expect("done"),
            target
        );
    }

    #[tokio::test]
    async fn rebinding_moves_extension_sidecars_and_resident_blobs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("old/s.jsonl");
        let dst = dir.path().join("new/s.jsonl");
        tokio::fs::create_dir_all(dir.path().join("old/extensions/goal"))
            .await
            .expect("dir");
        tokio::fs::create_dir_all(dir.path().join("old/resident-blobs/s1"))
            .await
            .expect("dir");
        let sidecar = format!(
            "{}.json",
            crate::session_sidecar_store::encoded_session_id("s1")
        );
        tokio::fs::write(dir.path().join("old/extensions/goal").join(&sidecar), b"{}")
            .await
            .expect("write");
        tokio::fs::write(dir.path().join("old/resident-blobs/s1/blob"), b"x")
            .await
            .expect("write");
        tokio::fs::write(
            &src,
            b"{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/old\"}\n",
        )
        .await
        .expect("write");
        move_session_file(&src, &dst, "/new").await.expect("move");
        assert!(
            dir.path()
                .join("new/extensions/goal")
                .join(&sidecar)
                .exists()
        );
        assert!(dir.path().join("new/resident-blobs/s1/blob").exists());
        assert!(!dir.path().join("old/resident-blobs/s1").exists());
    }

    #[tokio::test]
    async fn the_recorded_repository_identity_is_the_newest_valid_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("s.jsonl");
        let root = "a".repeat(40);
        tokio::fs::write(
            &file,
            format!(
                "{}\n{}\n",
                serde_json::json!({"type":"session","id":"s1"}),
                serde_json::json!({"type":"custom","customType":"repository-identity","data":{"rootCommits":[root]}})
            ),
        )
        .await
        .expect("write");
        let identity = read_recorded_repository_identity(&file)
            .await
            .expect("identity");
        assert_eq!(identity.root_commits, vec![root]);
    }

    #[tokio::test]
    async fn move_session_file_updates_cwd_and_preserves_rest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("source.jsonl");
        let dst = dir.path().join("sub").join("target.jsonl");
        tokio::fs::write(&src, b"{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/old\"}\n{\"type\":\"message\",\"content\":\"hi\"}\n").await.expect("write");
        let result = move_session_file(&src, &dst, "/new").await.expect("move");
        assert_eq!(result, dst);
        assert!(!src.exists());
        assert!(dst.exists());
        let content = tokio::fs::read_to_string(&dst).await.expect("read");
        assert!(content.contains("\"cwd\":\"/new\""));
        assert!(content.contains("\"content\":\"hi\""));
    }

    #[tokio::test]
    async fn move_session_file_preserves_header_metadata_and_fork_provenance() {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("source.jsonl");
        let dst = dir.path().join("sub").join("target.jsonl");
        let original_header = r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-10-16T00:00:00.000Z","cwd":"/old","parentSession":"/p.jsonl","forkedFromLeafId":"leaf-1"}"#;
        tokio::fs::write(
            &src,
            format!("{original_header}\n{{\"type\":\"message\",\"content\":\"hi\"}}\n"),
        )
        .await
        .expect("write");
        let result = move_session_file(&src, &dst, "/new").await.expect("move");
        assert_eq!(result, dst);
        let content = tokio::fs::read_to_string(&dst).await.expect("read");
        let first_line = content.lines().next().expect("first line");
        let parsed: serde_json::Value = serde_json::from_str(first_line).expect("json");
        assert_eq!(parsed["cwd"], "/new");
        assert_eq!(parsed["version"], 3);
        assert_eq!(parsed["timestamp"], "2026-10-16T00:00:00.000Z");
        assert_eq!(parsed["parentSession"], "/p.jsonl");
        assert_eq!(parsed["forkedFromLeafId"], "leaf-1");
    }
}
