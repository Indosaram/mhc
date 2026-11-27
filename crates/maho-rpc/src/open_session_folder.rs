//! Port of senpi `packages/coding-agent/src/modes/rpc/open-session-folder.ts`.
//!
//! The folder an `open_session` works in (senpi#6427): an explicit folder wins; otherwise a
//! resumed session file uses the folder recorded in its header, never silently falling back to
//! the host's default if resolution fails or times out.

use maho_core::session_header_cwd::read_header_cwd;
use std::path::{Path, PathBuf};

/// Async moved-path resolver abstraction matching `createMovedPathProbe`.
#[async_trait::async_trait]
pub trait MovedPathProbe: Send + Sync {
    async fn resolve(&self, session_path: &Path) -> Option<PathBuf>;
    fn timed_out(&self) -> bool;
}

/// Default no-op probe when no desktop moved-path guard mapping exists.
#[derive(Default)]
pub struct DirectMovedPathProbe;

#[async_trait::async_trait]
impl MovedPathProbe for DirectMovedPathProbe {
    async fn resolve(&self, _session_path: &Path) -> Option<PathBuf> {
        None
    }

    fn timed_out(&self) -> bool {
        false
    }
}

pub async fn open_session_folder_with_probe(
    cwd: Option<&str>,
    session_path: Option<&str>,
    host_default: &str,
    probe: &dyn MovedPathProbe,
) -> Result<String, String> {
    if let Some(cwd) = cwd {
        return Ok(cwd.to_owned());
    }
    let Some(session_path) = session_path else {
        return Ok(host_default.to_owned());
    };
    let path = Path::new(session_path);
    if !path.is_absolute() {
        return Ok(host_default.to_owned());
    }
    let mapped = probe.resolve(path).await;
    if probe.timed_out() {
        return Err(format!(
            "{session_path} could not be resolved: a filesystem step did not answer in time. Open it again, or name its folder with cwd."
        ));
    }
    let target = mapped.as_deref().unwrap_or(path);
    let recorded = read_header_cwd(target).await.map_err(|e| e.to_string())?;
    match recorded {
        Some(dir) if Path::new(&dir).is_absolute() => Ok(dir),
        _ => Ok(host_default.to_owned()),
    }
}

pub async fn open_session_folder(
    cwd: Option<&str>,
    session_path: Option<&str>,
    host_default: &str,
) -> Result<String, String> {
    open_session_folder_with_probe(cwd, session_path, host_default, &DirectMovedPathProbe).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    struct TimeoutProbe;
    #[async_trait::async_trait]
    impl MovedPathProbe for TimeoutProbe {
        async fn resolve(&self, _: &Path) -> Option<PathBuf> {
            None
        }
        fn timed_out(&self) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn prefers_explicit_cwd_then_session_header_and_refuses_timeout() {
        assert_eq!(
            open_session_folder(Some("/explicit"), Some("/missing"), "/host")
                .await
                .expect("explicit"),
            "/explicit"
        );
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("sess.jsonl");
        let mut f = std::fs::File::create(&file).expect("create");
        writeln!(
            f,
            "{{\"type\":\"session\",\"id\":\"s1\",\"cwd\":\"/from-header\"}}"
        )
        .expect("write");
        assert_eq!(
            open_session_folder(None, Some(&file.to_string_lossy()), "/host")
                .await
                .expect("header"),
            "/from-header"
        );
        let err = open_session_folder_with_probe(
            None,
            Some(&file.to_string_lossy()),
            "/host",
            &TimeoutProbe,
        )
        .await
        .expect_err("timeout");
        assert!(err.contains("could not be resolved"));
    }
}
