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
}

pub async fn read_session_cwd(session_file: impl AsRef<Path>) -> Result<Option<String>, std::io::Error> {
    let (header, _) = read_header_line(session_file.as_ref()).await?;
    Ok(header.cwd.filter(|s| !s.is_empty()))
}

async fn read_header_line(session_file: &Path) -> Result<(SessionHeaderLine, String), std::io::Error> {
    let content = tokio::fs::read_to_string(session_file).await?;
    let (first_line, rest) = match content.find('\n') {
        Some(pos) => (&content[..pos], content[pos + 1..].to_string()),
        None => (content.as_str(), String::new()),
    };

    let header: SessionHeaderLine = serde_json::from_str(first_line)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("Cannot rebind session: {error}")))?;
    if header.entry_type != "session" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Cannot rebind session: {} does not start with a session header", session_file.display()),
        ));
    }
    Ok((header, rest))
}

async fn read_header_before_lock(source: &Path, target: &Path) -> Result<Option<SessionHeaderLine>, std::io::Error> {
    match read_header_line(source).await {
        Ok((header, _)) => Ok(Some(header)),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound && target != source && target.exists() => Ok(None),
        Err(err) => Err(err),
    }
}

async fn move_session_file(source: &Path, target: &Path, cwd: &str) -> Result<std::path::PathBuf, std::io::Error> {
    if target != source && !source.exists() && target.exists() {
        return Ok(target.to_path_buf());
    }
    let (mut header, rest) = read_header_line(source).await?;
    if target != source && target.exists() {
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
    tokio::fs::rename(&temp_path, target).await?;
    if target != source && source.exists() {
        let _ = tokio::fs::remove_file(source).await;
    }
    Ok(target.to_path_buf())
}

pub async fn rebind_session_file(
    source_path: &Path,
    target_cwd: &Path,
    session_dir: Option<&Path>,
    _move_lock_wait_ms: Option<u64>,
) -> Result<std::path::PathBuf, std::io::Error> {
    let source = std::fs::canonicalize(source_path).unwrap_or_else(|_| source_path.to_path_buf());
    let cwd = std::fs::canonicalize(target_cwd).unwrap_or_else(|_| target_cwd.to_path_buf());
    let default_dir = crate::session_manager::get_default_session_dir(&cwd.to_string_lossy());
    let target_dir = session_dir
        .map(|d| d.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from(default_dir));

    let filename = source.file_name().unwrap_or_default();
    let target = target_dir.join(filename);

    if target != source && !source.exists() && target.exists() {
        return Ok(target);
    }

    let header = read_header_before_lock(&source, &target).await?;
    if header.is_none() {
        return Ok(target);
    }

    move_session_file(&source, &target, &cwd.to_string_lossy()).await
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
        let source = Source { cwd: dir.path().to_string_lossy().into_owned(), session_file: Some("/s.jsonl".into()) };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());
    }

    #[test]
    fn a_missing_session_file_is_not_an_issue() {
        let source = Source { cwd: "/definitely/missing".into(), session_file: None };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());
    }

    #[test]
    fn a_missing_cwd_is_reported_with_the_fallback() {
        let source = Source { cwd: "/definitely/missing".into(), session_file: Some("/s.jsonl".into()) };
        let issue = get_missing_session_cwd_issue(&source, "/fallback").expect("issue");
        assert_eq!(issue.session_cwd, "/definitely/missing");
        assert_eq!(issue.fallback_cwd, "/fallback");
        assert!(format_missing_session_cwd_error(&issue).contains("Session file: /s.jsonl"));
        assert!(format_missing_session_cwd_prompt(&issue).contains("continue in current cwd"));
    }

    #[test]
    fn assert_returns_the_typed_error() {
        let source = Source { cwd: "/definitely/missing".into(), session_file: Some("/s.jsonl".into()) };
        assert!(assert_session_cwd_exists(&source, "/fallback").is_err());
    }

    #[tokio::test]
    async fn read_session_cwd_reads_header() {
        let dir = tempfile::tempdir().expect("tempdir");
        let session_file = dir.path().join("session.jsonl");
        tokio::fs::write(&session_file, b"{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/my/project\"}\n{\"type\":\"message\"}\n").await.expect("write");
        let cwd = read_session_cwd(&session_file).await.expect("read").expect("cwd");
        assert_eq!(cwd, "/my/project");
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
}
