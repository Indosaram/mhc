use super::context::{TerminalToolResult, error_result, noticed_result};
use super::spawn::describe_exit;
use crate::output_format::format_terminal_tool_output;
use crate::runtime_session::{
    DeltaRead, RuntimeError, TerminalRuntimeSession, TerminalScreenSnapshot,
};
use crate::shared::safe_reg_exp;
use maho_pty::PtyExit;
pub fn bash_output_schema() -> serde_json::Value {
    serde_json::json!({"type":"object","properties":{"bash_id":{"type":"string","description":"Session id returned by a run_in_background bash call."},"filter":{"type":"string","description":"Only return output lines matching this regex."},"view":{"type":"string","enum":["log","screen"],"description":"'log' returns new raw output (default); 'screen' returns the rendered xterm grid."}},"required":["bash_id"]})
}
/// What bash-output.ts reads of a `TerminalRuntimeSession`: `exited`/`exitResult`, `snapshot()` and
/// `readDelta()`. The upstream peek suite drives the tool through a fake runtime with exactly these.
pub trait BashOutputRuntime {
    /// `runtime.exited`: the session has exited (an exit that settled with an error counts).
    fn exited(&self) -> Result<bool, RuntimeError>;
    /// `runtime.exitResult`: the settled exit, absent while running or when the exit errored.
    fn exit_result(&self) -> Result<Option<PtyExit>, RuntimeError>;
    fn snapshot(&self) -> Result<TerminalScreenSnapshot, RuntimeError>;
    fn read_delta(&self) -> Result<DeltaRead, RuntimeError>;
}
impl BashOutputRuntime for TerminalRuntimeSession {
    fn exited(&self) -> Result<bool, RuntimeError> {
        TerminalRuntimeSession::exited(self)
    }
    fn exit_result(&self) -> Result<Option<PtyExit>, RuntimeError> {
        // An exit that settled with an error is exited without an exit result (upstream `exitResult` null).
        match TerminalRuntimeSession::exit_result(self) {
            Err(RuntimeError::Pty(_)) => Ok(None),
            result => result,
        }
    }
    fn snapshot(&self) -> Result<TerminalScreenSnapshot, RuntimeError> {
        TerminalRuntimeSession::snapshot(self)
    }
    fn read_delta(&self) -> Result<DeltaRead, RuntimeError> {
        TerminalRuntimeSession::read_delta(self)
    }
}
/// bash-output.ts `statusLine`.
fn status_line(runtime: &impl BashOutputRuntime) -> Result<String, RuntimeError> {
    if !runtime.exited()? {
        return Ok("status: running".to_owned());
    }
    let exit = runtime.exit_result()?;
    let status = describe_exit(exit.as_ref()).unwrap_or_else(|| "exited".to_owned());
    Ok(match exit.and_then(|exit| exit.exit_code) {
        Some(code) => format!("status: {status} exit_code: {code}"),
        None => format!("status: {status}"),
    })
}
/// bash-output.ts `applyFilter`: an invalid pattern leaves the text unfiltered.
fn apply_filter(text: String, filter: Option<&str>) -> String {
    let Some(regex) = filter
        .filter(|filter| !filter.is_empty())
        .and_then(safe_reg_exp)
    else {
        return text;
    };
    text.split('\n')
        .filter(|line| regex.is_match(line).unwrap_or(false))
        .collect::<Vec<_>>()
        .join("\n")
}
pub fn execute_bash_output<R: BashOutputRuntime>(
    runtime: Option<&R>,
    id: &str,
    filter: Option<&str>,
) -> Result<TerminalToolResult, RuntimeError> {
    execute_bash_output_view(runtime, id, filter, false)
}
pub fn execute_bash_output_view<R: BashOutputRuntime>(
    runtime: Option<&R>,
    id: &str,
    filter: Option<&str>,
    screen: bool,
) -> Result<TerminalToolResult, RuntimeError> {
    let Some(runtime) = runtime else {
        return Ok(error_result(format!(
            "No terminal session found with id: {id}"
        )));
    };
    let status = status_line(runtime)?;
    if screen {
        return Ok(super::context::text_result(format!(
            "{status}\n{}",
            runtime.snapshot()?.visible_grid.join("\n").trim_end()
        )));
    }
    let delta = runtime.read_delta()?;
    let formatted = format_terminal_tool_output(&apply_filter(delta.text, filter));
    let dropped_notice = if delta.dropped_chars > 0 {
        Some(format!("[{} earlier chars dropped]", delta.dropped_chars))
    } else {
        None
    };
    let dropped = dropped_notice
        .as_ref()
        .map(|notice| format!("{notice}\n"))
        .unwrap_or_default();
    let text = format!(
        "{status}\n{dropped}{}",
        if formatted.text.is_empty() {
            "(no new output)"
        } else {
            &formatted.text
        }
    );
    Ok(noticed_result(
        &text,
        &[dropped_notice.as_deref(), formatted.marker.as_deref()],
    ))
}
pub fn with_monitor_state(
    mut result: TerminalToolResult,
    id: &str,
    paused: bool,
    dropped: usize,
) -> TerminalToolResult {
    let dropped = if paused { dropped } else { 0 };
    if paused && let Some(part) = result.content.first_mut() {
        let note = if dropped > 0 {
            format!(
                "monitor muted - {dropped} line(s) dropped while muted; run monitor({{ action: \"rearm\", bash_id: \"{id}\" }}) to resume."
            )
        } else {
            format!(
                "monitor muted; run monitor({{ action: \"rearm\", bash_id: \"{id}\" }}) to resume."
            )
        };
        part.text = format!("{note}\n{}", part.text);
    }
    let details = result.details.get_or_insert_with(Default::default);
    details.insert("monitorMuted".to_owned(), serde_json::json!(paused));
    details.insert("mutedDropped".to_owned(), serde_json::json!(dropped));
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    /// bash-output-peek.test.ts `FakeRuntime`: a delta drained on read, a fixed two-row screen and a
    /// settable exit. bash_output is a pure non-blocking peek over exactly this surface.
    #[derive(Default)]
    struct FakeRuntime {
        exited: bool,
        exit_result: Option<PtyExit>,
        output: std::cell::RefCell<String>,
        dropped: usize,
    }
    impl FakeRuntime {
        fn emit(&self, text: &str) {
            self.output.borrow_mut().push_str(text);
        }
    }
    impl BashOutputRuntime for FakeRuntime {
        fn exited(&self) -> Result<bool, RuntimeError> {
            Ok(self.exited)
        }
        fn exit_result(&self) -> Result<Option<PtyExit>, RuntimeError> {
            Ok(self.exit_result.clone())
        }
        fn snapshot(&self) -> Result<TerminalScreenSnapshot, RuntimeError> {
            Ok(TerminalScreenSnapshot {
                cols: 20,
                rows: 2,
                visible_grid: vec!["screen-row-1".to_owned(), "screen-row-2".to_owned()],
                scrollback: vec![],
                cursor: (0, 0),
            })
        }
        fn read_delta(&self) -> Result<DeltaRead, RuntimeError> {
            Ok(DeltaRead {
                text: std::mem::take(&mut *self.output.borrow_mut()),
                dropped_chars: self.dropped,
            })
        }
    }
    /// createFixture: only `bash-1` resolves to the runtime.
    fn peek(
        runtime: &FakeRuntime,
        id: &str,
        filter: Option<&str>,
        screen: bool,
    ) -> TerminalToolResult {
        execute_bash_output_view((id == "bash-1").then_some(runtime), id, filter, screen).unwrap()
    }
    fn first_text(result: &TerminalToolResult) -> &str {
        result.content.first().map_or("", |part| part.text.as_str())
    }
    #[test]
    fn peek_returns_status_and_new_output_immediately() {
        let runtime = FakeRuntime::default();
        runtime.emit("line one\nline two\n");
        let result = peek(&runtime, "bash-1", None, false);
        assert_eq!(result.is_error, None);
        assert_eq!(first_text(&result), "status: running\nline one\nline two");
    }
    #[test]
    fn peek_reports_no_new_output_when_nothing_arrived() {
        let runtime = FakeRuntime::default();
        assert_eq!(
            first_text(&peek(&runtime, "bash-1", None, false)),
            "status: running\n(no new output)"
        );
    }
    #[test]
    fn peek_applies_the_filter_regex() {
        let runtime = FakeRuntime::default();
        runtime.emit("drop this\nkeep this\n");
        let result = peek(&runtime, "bash-1", Some("keep"), false);
        assert!(first_text(&result).contains("keep this"));
        assert!(!first_text(&result).contains("drop this"));
    }
    #[test]
    fn peek_renders_the_screen_view_without_consuming_the_log() {
        let runtime = FakeRuntime::default();
        runtime.emit("pending\n");
        let result = peek(&runtime, "bash-1", None, true);
        assert_eq!(
            first_text(&result),
            "status: running\nscreen-row-1\nscreen-row-2"
        );
        assert_eq!(*runtime.output.borrow(), "pending\n");
    }
    #[test]
    fn peek_reports_exit_status_for_a_finished_session() {
        let runtime = FakeRuntime {
            exited: true,
            exit_result: Some(PtyExit {
                exit_code: Some(3),
                cancelled: false,
                timed_out: false,
            }),
            ..Default::default()
        };
        assert_eq!(
            first_text(&peek(&runtime, "bash-1", None, false)),
            "status: exited_3 exit_code: 3\n(no new output)"
        );
        // An exited session without an exit result (a failed exit wait) reads `status: exited`.
        let errored = FakeRuntime {
            exited: true,
            ..Default::default()
        };
        assert_eq!(
            first_text(&peek(&errored, "bash-1", None, false)),
            "status: exited\n(no new output)"
        );
        let killed = FakeRuntime {
            exited: true,
            exit_result: Some(PtyExit {
                exit_code: None,
                cancelled: true,
                timed_out: false,
            }),
            ..Default::default()
        };
        assert_eq!(
            first_text(&peek(&killed, "bash-1", None, false)),
            "status: killed\n(no new output)"
        );
    }
    #[test]
    fn peek_errors_for_an_unknown_bash_id() {
        let result = peek(&FakeRuntime::default(), "bash-404", None, false);
        assert_eq!(result.is_error, Some(true));
        assert!(first_text(&result).contains("bash-404"));
    }
    #[test]
    fn peek_marks_dropped_chars_as_a_model_only_notice() {
        let runtime = FakeRuntime {
            dropped: 7,
            ..Default::default()
        };
        runtime.emit("tail\n");
        let result = peek(&runtime, "bash-1", None, false);
        let parts = result
            .content
            .iter()
            .map(|part| (part.text.as_str(), part.audience.is_some()))
            .collect::<Vec<_>>();
        assert_eq!(
            parts,
            [
                ("status: running", false),
                ("[7 earlier chars dropped]", true),
                ("tail", false)
            ]
        );
    }
    #[test]
    fn muted_screen_and_log_views_carry_the_note_and_metadata() {
        let runtime = FakeRuntime::default();
        runtime.emit("KEEP_ONE\nKEEP_TWO\n");
        let screen = with_monitor_state(peek(&runtime, "bash-1", None, true), "bash-1", true, 2);
        assert_eq!(
            serde_json::Value::Object(screen.details.clone().unwrap()),
            serde_json::json!({"monitorMuted":true,"mutedDropped":2})
        );
        assert!(first_text(&screen).starts_with("monitor muted - 2 line(s) dropped while muted; run monitor({ action: \"rearm\", bash_id: \"bash-1\" }) to resume.\nstatus: running\n"));
        let log = with_monitor_state(peek(&runtime, "bash-1", None, false), "bash-1", true, 2);
        assert!(first_text(&log).contains("KEEP_ONE"));
        assert!(first_text(&log).contains("KEEP_TWO"));
        let resumed = with_monitor_state(peek(&runtime, "bash-1", None, false), "bash-1", false, 0);
        assert_eq!(
            serde_json::Value::Object(resumed.details.clone().unwrap()),
            serde_json::json!({"monitorMuted":false,"mutedDropped":0})
        );
        assert!(!first_text(&resumed).contains("muted"));
        let quiet = with_monitor_state(peek(&runtime, "bash-1", None, false), "bash-1", true, 0);
        assert!(first_text(&quiet).starts_with(
            "monitor muted; run monitor({ action: \"rearm\", bash_id: \"bash-1\" }) to resume.\n"
        ));
    }
    #[test]
    fn screen_projection_applies_cursor_edits_without_consuming_log() -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "screen",
            crate::shared::test_sh_options("stty -echo; printf 'old\rnew\\033[K'").size(20, 4),
        )?;
        runtime.wait(std::time::Duration::from_secs(5))?;
        let result = execute_bash_output_view(Some(&runtime), "bash_1", Some("absent"), true)?;
        let expected = if cfg!(windows) { "oldnew" } else { "new" };
        assert_eq!(result.content[0].text.lines().last(), Some(expected));
        assert_eq!(runtime.snapshot()?.visible_grid[0], expected);
        // ConPTY re-renders the line: the carriage return is gone but the erase-line survives.
        let expected_delta = if cfg!(windows) {
            "oldnew\x1b[K"
        } else {
            "old\rnew\x1b[K"
        };
        assert_eq!(runtime.read_delta()?.text, expected_delta);
        runtime.dispose()
    }
    #[test]
    fn live_output_filter_consumes_delta_once_and_invalid_regex_keeps_output()
    -> Result<(), RuntimeError> {
        let runtime = TerminalRuntimeSession::start(
            "output",
            crate::shared::test_sh_options("stty -echo; printf 'keep\\ndrop\\n'"),
        )?;
        runtime.wait(std::time::Duration::from_secs(5))?;
        let result = execute_bash_output(Some(&runtime), "bash_1", Some("keep"))?;
        assert!(result.content[0].text.contains("keep"));
        assert!(!result.content[0].text.contains("drop"));
        assert!(
            execute_bash_output(Some(&runtime), "bash_1", None)?.content[0]
                .text
                .contains("(no new output)")
        );
        runtime.dispose()?;
        let runtime = TerminalRuntimeSession::start(
            "output",
            crate::shared::test_sh_options("stty -echo; printf 'unfiltered\\n'"),
        )?;
        runtime.wait(std::time::Duration::from_secs(5))?;
        assert!(
            execute_bash_output(Some(&runtime), "bash_2", Some("["))?.content[0]
                .text
                .contains("unfiltered")
        );
        runtime.dispose()
    }
    #[test]
    fn muted_log_metadata_reports_burned_lines_only_while_paused() {
        let paused = with_monitor_state(
            super::super::context::text_result("output"),
            "mon_saved",
            true,
            3,
        );
        assert_eq!(paused.details.as_ref().unwrap()["monitorMuted"], true);
        assert_eq!(paused.details.as_ref().unwrap()["mutedDropped"], 3);
        let live = with_monitor_state(
            super::super::context::text_result("output"),
            "mon_saved",
            false,
            3,
        );
        assert_eq!(live.details.as_ref().unwrap()["monitorMuted"], false);
        assert_eq!(live.details.as_ref().unwrap()["mutedDropped"], 0);
        assert_eq!(live.content[0].text, "output");
    }
}
