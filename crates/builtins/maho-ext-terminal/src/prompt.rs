use crate::tools::foreground_window::DEFAULT_FOREGROUND_WINDOW_SECONDS;
use maho_ext_bash_timeout::timeout::BASH_DEFAULT_TIMEOUT_SECONDS;

/// Controls whether shared prompt sections render in compact diet mode or pre-diet full mode (`senpi#5665`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SharedSections {
    #[default]
    Diet,
    Full,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalPromptOptions {
    /// True when the session routes shell tools through eval cells, so call shapes must be `tool.<name>(`.
    pub eval_only: bool,
    /// Native bash replaces PTY bash, but monitor and its terminal companions remain available.
    pub pty_bash: bool,
    /// Seconds a foreground `bash` call blocks before it auto-detaches (`PI_BASH_FOREGROUND_SECONDS`).
    pub foreground_window_seconds: u64,
    /// `timeout` applied when a `bash` call sets none (`PI_BASH_DEFAULT_TIMEOUT_SECONDS`).
    pub default_timeout_seconds: u64,
    /// `Full` renders the pre-diet section, whose timeout facts live in the bash-timeout policy instead (`senpi#5665`).
    pub shared_sections: SharedSections,
}

impl Default for TerminalPromptOptions {
    fn default() -> Self {
        Self {
            eval_only: false,
            pty_bash: true,
            foreground_window_seconds: DEFAULT_FOREGROUND_WINDOW_SECONDS.trunc() as u64,
            default_timeout_seconds: BASH_DEFAULT_TIMEOUT_SECONDS,
            shared_sections: SharedSections::Diet,
        }
    }
}

/// Prompt guidance for the terminal tools.
pub fn build_terminal_prompt_section(eval_only: bool) -> String {
    build_terminal_prompt_section_with_options(TerminalPromptOptions {
        eval_only,
        ..Default::default()
    })
}

/// Prompt guidance for the terminal tools with explicit window, timeout, `pty_bash`, and preset controls.
pub fn build_terminal_prompt_section_with_options(options: TerminalPromptOptions) -> String {
    if options.shared_sections == SharedSections::Full {
        return build_full_terminal_prompt_section(&options);
    }
    let pty_bash = options.pty_bash;
    let bash = if options.eval_only {
        "tool.bash"
    } else {
        "bash"
    };
    let monitor = if options.eval_only {
        "tool.monitor"
    } else {
        "monitor"
    };
    let window = options.foreground_window_seconds;
    let default_timeout = options.default_timeout_seconds;
    let eval_only_reach = if !options.eval_only {
        ""
    } else if pty_bash {
        "`bash`, `monitor`: eval-only, call as `tool.bash({ command })` / `tool.monitor(...)`; params via `tool_schema(\"bash\")` / `tool_schema(\"monitor\")`. "
    } else {
        "`monitor`: eval-only, call as `tool.monitor(...)`; params via `tool_schema(\"monitor\")`. "
    };
    let bash_guidance = if pty_bash {
        format!(
            "`bash` = PTY. Never tmux, plain `&`, or `nohup` (PTY hangup or call timeout kills them). Only `{bash}({{ command, run_in_background: true }})` -> session outliving the call, returns `bash_id`. Foreground call blocks ~{window}s then auto-detaches alive to background. `timeout` (seconds, default {default_timeout}) = process kill deadline, not output wait; background sessions ignore it."
        )
    } else {
        String::new()
    };
    let opening_raw = format!("{eval_only_reach}{bash_guidance}");
    let opening = opening_raw.trim_end();
    let opening_block = if opening.is_empty() {
        String::new()
    } else {
        format!("{opening}\n\n")
    };
    let longer_job = if pty_bash {
        "longer job: `run_in_background` + watch only"
    } else {
        "longer job: `persistent: true` + stop via `kill_bash`"
    };
    format!(
        r#"
## Persistent terminal sessions

{opening_block}- `bash_output({{ bash_id, filter, view }})`: non-blocking peek (new output / status line / `view: "screen"` for TUI). Completion = notification with exit code + tail. Peek to steer, never to wait.
- Waiting on observable state (CI, build, log pattern, deploy, file, other session): register `{monitor}`, keep working. No sleep/poll in cells, no repeated `bash_output`; sleep loops live inside monitor command. Exactly one source per call:
  - `{monitor}({{ description, command, filter?, timeout_ms?, persistent? }})`: lines matching `filter` -> events; exit -> summary. `timeout_ms` KILLS watched command; {longer_job}. One-shot gate: `until <cond>; do sleep 1; done; printf 'READY\n'` + `filter: "^READY$"`. Stream: `tail -n 0 -F <log> | grep --line-buffered <pat>` + `persistent: true`. Filter noise at source; stop via `kill_bash`.
  - `{monitor}({{ description, path, event?, persistent? }})`: one regular file, parent dir must exist, fires once, no `filter`. `"create"` (default) fires only for a file that appears after registration; existing file -> `event: "modify"`; run creates the dir too -> `command` poll.
  - `{monitor}({{ type: "rpc", target, description, events?, exclude?, filter?, persistent? }})`: `target` = `"children"` / `"all"` / one session id, path, `st_` id / `<device>/<session>` on another device (e.g. `"laptop/abc123"`). Status changes + effect receipts -> events; `events: "all"` adds content; `exclude` drops kinds.
- `persistent: true`: no deadline, survives restart (command re-run with `SENPI_MONITOR_RESTORED=1`, file rescanned), expires 7 days. Baseline in `$SENPI_MONITOR_STATE_DIR` so a move during downtime (<= `$SENPI_MONITOR_DOWNTIME_MS` ms) still reported. Loop failures non-fatal: exited watch not restarted.
- Repeated monitor-only wakes pause that noisy monitor only. `{monitor}({{ action: "rearm", bash_id }})` resumes one; `{monitor}({{ action: "rearm" }})` all. Completion and real user input also resume.
- `bash_input({{ bash_id, input, keys, submit }})`: stdin or keys (`["ctrl+c"]`, `["enter"]`). `bash_resize({{ bash_id, cols, rows }})`: reflow TUI. `kill_bash({{ bash_id }})` / `{{ all: true }}`: tear down session tree.
"#
    )
}

/// The section as worded before the `senpi#5335` diet, for presets that render full shared sections (`senpi#5665`).
fn build_full_terminal_prompt_section(options: &TerminalPromptOptions) -> String {
    let bash = if options.eval_only {
        "tool.bash"
    } else {
        "bash"
    };
    let monitor = if options.eval_only {
        "tool.monitor"
    } else {
        "monitor"
    };
    let bash_guidance = if !options.pty_bash {
        String::new()
    } else {
        format!(
            r#"The `bash` tool is PTY-backed. For long-running or interactive work, do NOT use tmux or
manual `&` backgrounding; use the built-in session tools:

- `{bash}({{ command, run_in_background: true }})` starts a persistent session and returns a
  `bash_id` immediately. Foreground calls still block and return output; their `timeout`
  (seconds) is a kill deadline. Background sessions ignore `timeout` and live until they
  exit or you call `kill_bash`. Backgrounding is not a session: a plain `&` job dies with the
  call's PTY hangup (native backend), `nohup` at best leaves an untracked orphan (no `bash_id`,
  no log) that the call's timeout still group-kills; only `run_in_background` creates a
  session that outlives the call.
"#
        )
    };
    let longer_jobs = if !options.pty_bash {
        "for longer jobs, set `persistent: true` and stop with `kill_bash`."
    } else {
        "for longer jobs, start with `run_in_background` and only watch them."
    };
    format!(
        r#"
## Persistent terminal sessions

{bash_guidance}- `bash_output({{ bash_id, filter, view }})` peeks at a session without blocking: new output
  since the last read, the status line, or a rendered full-screen snapshot of TUIs via
  `view: "screen"`. Completion arrives as a notification carrying the exit code and output
  tail: peeking is for steering, never for waiting.
- For observable state (CI checks, builds, log patterns, deploys, files or other sessions),
  register `{monitor}` and keep working instead of foreground sleeping/polling or repeatedly
  calling `bash_output`. Sleep loops belong inside the monitor command.
- `{monitor}` subscribes you to a change. Pass exactly one source (`command`, `path` or
  `target`; one branch per call, never two):
  - `{monitor}({{ description, command, filter?, timeout_ms?, persistent? }})` watches a command:
    newline-terminated PTY output lines (stderr included) matching `filter` arrive as injected
    events while you keep working; command exit always delivers a summary. `timeout_ms` is
    that watch's own deadline: it KILLS the watched command when it fires (the summary says
    `killed at deadline`); {longer_jobs} One-shot gate: wait
    inside the command and print one sentinel (`until <cond>; do sleep 1; done;
    printf 'READY\n'`) with `filter: "^READY$"`. Stream: `tail -n 0 -F <log> | grep --line-buffered <pat>`
    with `persistent: true`. Filter noise at the
    source and stop with `kill_bash`. Persistent command watches are re-run after a restart with
    `SENPI_MONITOR_RESTORED=1`; keep any baseline in `$SENPI_MONITOR_STATE_DIR` so a move during
    downtime (at most `$SENPI_MONITOR_DOWNTIME_MS` ms) is still reported:
    `b="$SENPI_MONITOR_STATE_DIR/base"; p=$(cat "$b" 2>/dev/null || git rev-parse origin/main | tee "$b"); while sleep 30; do git fetch -q || true; n=$(git rev-parse origin/main); [ "$n" != "$p" ] && {{ echo "MOVED $p..$n"; p=$n; echo "$p" > "$b"; }}; done`.
    A watch whose command exits is not restarted, so keep failures inside the loop non-fatal.
  - `{monitor}({{ description, path, event?, persistent? }})` natively watches one regular file and
    fires once; prefer it over a shell poll loop. `"create"` (default) fires only when the file
    appears after registration, so watch an already-existing file with `"modify"`; registration needs
    the parent directory to exist already, so poll with a `command` when the run creates that
    directory too. This branch takes no `filter`.
  - `{monitor}({{ type: "rpc", target, description, events?, exclude?, filter?, persistent? }})` watches other
    sessions over every RPC host and terminal: `target: "children"` (sessions you spawned, including
    later ones), `"all"`, or one session id, path or `st_` task id; a session on another device is
    `<device>/<session>` (e.g. `"laptop/abc123"`). The result lists each matched
    session as it is now; then each status change (`rpc <label> status running -> idle ...`) and
    effect receipt arrives as an event. `events: "all"` adds their content, streaming collapsed to one
    line per 2 s; `exclude` drops kinds. Watching never attaches to a session.
  A standing watch is marked `persistent: true`: it has no deadline, survives a session restart
  (the command is re-run once, the file rescanned and any change missed while detached reported),
  expires 7 days after creation, and is accounted for in one restart-report line on session start.
  Identical updates are deduped; repeated monitor-only wakes pause the noisy monitor(s) that
  caused them, not all monitors. Completion still wakes the session, and
  `{monitor}({{ action: "rearm", bash_id }})` resumes one while `{monitor}({{ action: "rearm" }})`
  resumes all paused monitors; real user input also resumes paused monitors.
- `bash_input({{ bash_id, input, keys, submit }})` sends stdin or named keys (e.g.
  `["ctrl+c"]`, `["enter"]`) to steer a REPL or interrupt a process.
- `bash_resize({{ bash_id, cols, rows }})` resizes the PTY so full-screen programs reflow.
- `kill_bash({{ bash_id }})` (or `{{ all: true }}`) tears the session tree down with no orphans.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::{
        SharedSections, TerminalPromptOptions, build_terminal_prompt_section,
        build_terminal_prompt_section_with_options,
    };

    #[test]
    fn eval_only_section_never_advertises_a_top_level_bash_tool() {
        let section = build_terminal_prompt_section(true);
        assert!(
            section.contains("`bash`, `monitor`: eval-only"),
            "{section}"
        );
        assert!(section.contains("`tool.bash({ command, run_in_background: true })`"));
        assert!(
            section.contains(
                "`tool.monitor({ description, command, filter?, timeout_ms?, persistent? })`"
            ),
            "{section}"
        );
    }

    #[test]
    fn direct_section_names_the_bash_tool() {
        let section = build_terminal_prompt_section(false);
        assert!(
            section.contains("Only `bash({ command, run_in_background: true })`"),
            "{section}"
        );
        assert!(!section.contains("tool.bash("));
    }

    #[test]
    fn pty_bash_false_omits_bash_guidance_and_full_mode_uses_pre_diet_copy() {
        let no_bash = build_terminal_prompt_section_with_options(TerminalPromptOptions {
            eval_only: true,
            pty_bash: false,
            ..Default::default()
        });
        assert!(no_bash.contains("`monitor`: eval-only"));
        assert!(!no_bash.contains("tool.bash("));
        assert!(no_bash.contains("longer job: `persistent: true` + stop via `kill_bash`"));

        let full = build_terminal_prompt_section_with_options(TerminalPromptOptions {
            eval_only: false,
            shared_sections: SharedSections::Full,
            ..Default::default()
        });
        assert!(full.contains("The `bash` tool is PTY-backed."));
        assert!(full.contains("`<device>/<session>`"));
    }
}
