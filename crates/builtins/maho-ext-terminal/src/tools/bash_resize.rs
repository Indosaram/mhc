use super::context::{TerminalToolResult, error_result, text_result};
use crate::runtime_session::TerminalRuntimeSession;
pub fn bash_resize_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"bash_id":{"type":"string","description":"Session id returned by a run_in_background bash call."},"cols":{"type":"number","description":"New PTY width in columns."},"rows":{"type":"number","description":"New PTY height in rows."}},"required":["bash_id","cols","rows"]})
}
/// What bash-resize.ts uses of a runtime: `exited`, then `session.resize` (whose failure is a note)
/// followed by `resizeScreen`; `TerminalRuntimeSession::resize` does both and returns the PTY's result.
pub trait BashResizeRuntime {
    fn exited(&self) -> Result<bool, crate::runtime_session::RuntimeError>;
    fn resize(&self, cols: u16, rows: u16) -> Result<(), crate::runtime_session::RuntimeError>;
}
impl BashResizeRuntime for TerminalRuntimeSession {
    fn exited(&self) -> Result<bool, crate::runtime_session::RuntimeError> {
        TerminalRuntimeSession::exited(self)
    }
    fn resize(&self, cols: u16, rows: u16) -> Result<(), crate::runtime_session::RuntimeError> {
        TerminalRuntimeSession::resize(self, cols, rows)
    }
}
pub fn execute_bash_resize<R: BashResizeRuntime + ?Sized>(
    runtime: Option<&R>,
    id: &str,
    cols: f64,
    rows: f64,
) -> TerminalToolResult {
    let Some(runtime) = runtime else {
        return error_result(format!("No terminal session found with id: {id}"));
    };
    match runtime.exited() {
        Ok(true) => return error_result(format!("Session {id} is not running; cannot resize.")),
        Err(error) => return error_result(error.to_string()),
        Ok(false) => {}
    }
    if !cols.is_finite() || !rows.is_finite() || cols < 1.0 || rows < 1.0 {
        return error_result("cols and rows must be finite integers >= 1.");
    }
    let cols = cols.trunc() as u16;
    let rows = rows.trunc() as u16;
    match runtime.resize(cols, rows) {
        Err(error) => text_result(format!("Resize note for {id}: {error}")),
        Ok(()) => text_result(format!("Resized {id} to {cols}x{rows}.")),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct FakeRuntime {
        exited: bool,
        note: Option<&'static str>,
        resized: std::cell::RefCell<Vec<(u16, u16)>>,
    }
    impl BashResizeRuntime for FakeRuntime {
        fn exited(&self) -> Result<bool, crate::runtime_session::RuntimeError> {
            Ok(self.exited)
        }
        fn resize(&self, cols: u16, rows: u16) -> Result<(), crate::runtime_session::RuntimeError> {
            self.resized.borrow_mut().push((cols, rows));
            match self.note {
                Some(note) => Err(crate::runtime_session::RuntimeError::Pty(
                    std::io::Error::other(note).into(),
                )),
                None => Ok(()),
            }
        }
    }
    #[test]
    fn resize_truncates_and_validates_dimensions_before_resizing() {
        let runtime = FakeRuntime::default();
        let result = execute_bash_resize(Some(&runtime), "bash_1", 100.9, 30.2);
        assert_eq!(result.is_error, None);
        assert_eq!(result.content[0].text, "Resized bash_1 to 100x30.");
        for (cols, rows) in [
            (0.0, 24.0),
            (80.0, 0.5),
            (f64::NAN, 24.0),
            (80.0, f64::INFINITY),
            (-1.0, 24.0),
        ] {
            let result = execute_bash_resize(Some(&runtime), "bash_1", cols, rows);
            assert_eq!(result.is_error, Some(true));
            assert_eq!(
                result.content[0].text,
                "cols and rows must be finite integers >= 1."
            );
        }
        assert_eq!(*runtime.resized.borrow(), [(100, 30)]);
    }
    #[test]
    fn pipe_fallback_note_is_informative_not_an_error() {
        let runtime = FakeRuntime {
            note: Some(
                "Cannot resize pipe fallback session to 100x30: child_process pipes are not a PTY; session is still valid.",
            ),
            ..Default::default()
        };
        let result = execute_bash_resize(Some(&runtime), "bash_1", 100.0, 30.0);
        assert_eq!(result.is_error, None);
        assert!(
            result.content[0]
                .text
                .starts_with("Resize note for bash_1: Cannot resize")
        );
        assert!(result.content[0].text.to_lowercase().contains("resize"));
    }
    #[test]
    fn missing_and_exited_sessions_are_errors_without_resizing() {
        let missing = execute_bash_resize::<FakeRuntime>(None, "bash_9", 80.0, 24.0);
        assert_eq!(missing.is_error, Some(true));
        assert_eq!(
            missing.content[0].text,
            "No terminal session found with id: bash_9"
        );
        let exited = FakeRuntime {
            exited: true,
            ..Default::default()
        };
        let result = execute_bash_resize(Some(&exited), "bash_1", f64::NAN, 24.0);
        assert_eq!(
            result.content[0].text,
            "Session bash_1 is not running; cannot resize."
        );
        assert!(exited.resized.borrow().is_empty());
    }
    /// xterm operation-history parity: a resize replays what the screen was fed, not the log buffer,
    /// so a session whose log was already read still reflows its wrapped output at the new width.
    #[tokio::test]
    async fn resize_reflows_screen_history_after_the_log_was_read()
    -> Result<(), crate::runtime_session::RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "reflow",
            crate::shared::test_sh_options("printf 'AAAAAAAAAABBBBBBBBBB'; read value").size(10, 4),
        )?;
        let (history, mut output) = runtime.subscribe_output()?;
        let mut observed = history;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !observed.contains("BBBBBBBBBB") {
                observed.push_str(&output.recv().await.unwrap());
            }
        })
        .await
        .unwrap();
        assert!(runtime.read_delta()?.text.contains("BBBBBBBBBB"));
        assert_eq!(
            runtime.snapshot()?.visible_grid[..2],
            ["AAAAAAAAAA", "BBBBBBBBBB"]
        );
        assert_eq!(
            execute_bash_resize(Some(&runtime), "bash_1", 5.0, 4.0).is_error,
            None
        );
        let snapshot = runtime.snapshot()?;
        assert_eq!((snapshot.cols, snapshot.rows), (5, 4));
        assert_eq!(snapshot.visible_grid, ["AAAAA", "AAAAA", "BBBBB", "BBBBB"]);
        assert_eq!(runtime.read_delta()?.text, "");
        runtime.dispose()
    }
    #[tokio::test]
    async fn resized_geometry_is_visible_inside_live_pty()
    -> Result<(), crate::runtime_session::RuntimeError> {
        let mut runtime = TerminalRuntimeSession::start(
            "geometry",
            crate::shared::test_sh_options("stty -echo; printf 'ready\\n'; read value; stty size"),
        )?;
        let (history, mut output) = runtime.subscribe_output()?;
        let mut observed = history;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !observed.contains("ready") {
                observed.push_str(&output.recv().await.unwrap());
            }
        })
        .await
        .unwrap();
        assert_eq!(
            execute_bash_resize(Some(&runtime), "bash_1", f64::NAN, 24.0).is_error,
            Some(true)
        );
        assert_eq!(
            execute_bash_resize(Some(&runtime), "bash_1", 100.0, 40.0).is_error,
            None
        );
        let mut exit = runtime.subscribe_exit();
        runtime.write(b"go\n")?;
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while exit.borrow_and_update().is_none() {
                exit.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(runtime.full_output()?.contains("40 100"));
        assert_eq!(
            execute_bash_resize(Some(&runtime), "bash_1", 80.0, 24.0).is_error,
            Some(true)
        );
        runtime.dispose()
    }
}
