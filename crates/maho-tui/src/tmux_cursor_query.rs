//! Port of senpi `packages/tui/src/tmux-cursor-query.ts`.
//!
//! tmux swallows private DECXCPR, so the pane cursor is sampled out of band. senpi takes one
//! reading, then a second one 10ms later on a timer; here the first reading is
//! [`query_tmux_cursor_position`] and the timer-driven second one is
//! [`finish_tmux_cursor_query`], scheduled by the owning terminal on its clock.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, LazyLock, mpsc};
use std::time::Duration;

use regex::Regex;

use crate::stdin_buffer::Clock;
use crate::terminal::CursorPosition;

/// `TmuxExecFile` (declared in senpi tmux-image-probe.ts; todo 9 reuses this alias).
pub type TmuxExecFile = Arc<dyn Fn(&str, &[String]) -> Result<String, String> + Send + Sync>;

const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
const SECOND_READ_DELAY_MS: u64 = 10;

static CURSOR_OUTPUT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d+) (\d+)$").expect("valid regex"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxCursorQuery {
    Done(Option<CursorPosition>),
    /// Take the second reading at clock time `due`.
    SecondReadAt {
        due: u64,
        first: CursorPosition,
    },
}

/// `execFileSync(file, args, { encoding: "utf8", timeout, stdio: ["ignore", "pipe", "ignore"] })`.
pub fn exec_file_sync(file: &str, args: &[String], timeout_ms: u64) -> Result<String, String> {
    let mut child = Command::new(file)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut stdout = child.stdout.take().ok_or_else(|| "no stdout".to_string())?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut out = String::new();
        let result = stdout.read_to_string(&mut out).map(|_| out);
        let _ = tx.send(result);
    });
    match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
        Ok(read) => {
            let status = child.wait().map_err(|e| e.to_string())?;
            let out = read.map_err(|e| e.to_string())?;
            if status.success() {
                Ok(out)
            } else {
                Err(format!("{file} exited with {status}"))
            }
        }
        Err(_) => {
            let _ = child.kill();
            let _ = child.wait();
            Err(format!("{file} timed out"))
        }
    }
}

fn read(
    pane: &str,
    deadline: u64,
    exec_file: Option<&TmuxExecFile>,
    clock: &dyn Clock,
) -> Option<CursorPosition> {
    let now = clock.now_ms();
    if deadline <= now {
        return None;
    }
    let remaining = deadline - now;
    let args: Vec<String> = [
        "display-message",
        "-p",
        "-t",
        pane,
        "#{cursor_y} #{cursor_x}",
    ]
    .iter()
    .map(|s| (*s).to_string())
    .collect();
    // Missing tmux, detached panes and command timeouts all leave placement unknown.
    let output = match exec_file {
        Some(exec) => exec("tmux", &args),
        None => exec_file_sync("tmux", &args, remaining),
    }
    .ok()?;
    let captures = CURSOR_OUTPUT.captures(output.trim())?;
    if clock.now_ms() >= deadline {
        return None;
    }
    let parse = |i: usize| {
        captures
            .get(i)?
            .as_str()
            .parse::<u64>()
            .ok()?
            .checked_add(1)
    };
    let row = parse(1)?;
    let column = parse(2)?;
    if row > MAX_SAFE_INTEGER || column > MAX_SAFE_INTEGER {
        return None;
    }
    Some(CursorPosition {
        row,
        column,
        page: None,
    })
}

pub fn query_tmux_cursor_position(
    pane: &str,
    deadline: u64,
    exec_file: Option<&TmuxExecFile>,
    clock: &dyn Clock,
) -> TmuxCursorQuery {
    let Some(first) = read(pane, deadline, exec_file, clock) else {
        return TmuxCursorQuery::Done(None);
    };
    let now = clock.now_ms();
    if deadline.saturating_sub(now) <= SECOND_READ_DELAY_MS {
        return TmuxCursorQuery::Done(None);
    }
    TmuxCursorQuery::SecondReadAt {
        due: now + SECOND_READ_DELAY_MS,
        first,
    }
}

pub fn finish_tmux_cursor_query(
    pane: &str,
    deadline: u64,
    exec_file: Option<&TmuxExecFile>,
    clock: &dyn Clock,
    first: CursorPosition,
) -> Option<CursorPosition> {
    let second = read(pane, deadline, exec_file, clock)?;
    (second.row == first.row && second.column == first.column).then_some(second)
}
