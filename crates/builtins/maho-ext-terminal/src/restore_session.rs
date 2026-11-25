//! Port of `restore-session.ts` (`senpi#4718`, `oh-my-openagent#14313`):
//! Stale-context retirement helpers for restore digest flushing and persistence teardown
//! (`closed` removes the manifest instead of suspending it).

use crate::context_lifetime::with_terminal_context;
use crate::terminal_manifest::TerminalManifestWriter;
use std::error::Error;

/// Flush a restore digest callback inside `with_terminal_context`, retiring the session's
/// persistence state (stopping the lease keeper and clearing queued digests) when the host
/// context error carries `STALE_EXTENSION_CONTEXT_ERROR_PREFIX`.
pub fn flush_digest_with_context<T, E: Error>(
    flush: impl FnOnce() -> Result<T, E>,
    retire: impl FnOnce(),
) -> Result<Option<T>, E> {
    with_terminal_context(flush, retire)
}

/// Persist shutdown or close state for `writer`: a graceful close (`closed = true`,
/// `oh-my-openagent#14313`) clears all monitors/backgrounds and deletes the manifest file
/// instead of marking monitors suspended.
pub async fn stop_persistence_writer(
    writer: &mut TerminalManifestWriter,
    closed: bool,
    now_ms: f64,
) {
    if closed {
        writer.record_closed().await;
    } else {
        writer.record_shutdown(now_ms).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_goal::stale_context::STALE_EXTENSION_CONTEXT_ERROR_PREFIX;
    use std::fmt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Debug)]
    struct TestError(String);
    impl fmt::Display for TestError {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl Error for TestError {}

    #[test]
    fn flush_digest_retires_on_stale_context_error_and_rethrows_others() {
        let retired = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&retired);
        let stale = flush_digest_with_context::<(), TestError>(
            || {
                Err(TestError(format!(
                    "{STALE_EXTENSION_CONTEXT_ERROR_PREFIX} gone"
                )))
            },
            move || flag.store(true, Ordering::SeqCst),
        );
        assert!(matches!(stale, Ok(None)));
        assert!(retired.load(Ordering::SeqCst));

        let retired2 = Arc::new(AtomicBool::new(false));
        let flag2 = Arc::clone(&retired2);
        let other = flush_digest_with_context::<(), TestError>(
            || Err(TestError("unrelated failure".to_owned())),
            move || flag2.store(true, Ordering::SeqCst),
        );
        assert!(other.is_err());
        assert!(!retired2.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn stop_persistence_writer_deletes_manifest_on_graceful_close() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut writer = TerminalManifestWriter::new(dir.path(), "s-close");
        let file = writer.store.file_path().to_owned();
        writer
            .record_background_start("bash_1", "sleep 10", 1.0, 2.0)
            .await;
        assert!(std::path::Path::new(&file).exists());

        stop_persistence_writer(&mut writer, false, 3.0).await;
        assert!(std::path::Path::new(&file).exists());

        stop_persistence_writer(&mut writer, true, 4.0).await;
        assert!(!std::path::Path::new(&file).exists());
    }
}
