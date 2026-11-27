//! Port of `packages/coding-agent/src/modes/rpc/machine-op/terminal-wait.ts`.
//!
//! `wait_for_command` and `TerminalResult`: holds at most `yield_ms` and answers on the first
//! event, never by polling. `last_seq` and `total_bytes` are byte offsets.

use crate::machine_op::live_handle::{
    After, HandleEvent, HandleExit, LiveHandle, MachineLifecycle, ended,
};
use maho_machine_ops::{AbortSignal, MachineOpError, OpErrorCode};
use serde_json::{Map, Value, json};
use tokio::sync::{broadcast, oneshot};

const WAIT_SEEN_MAX: usize = 64 * 1024;
/// The most output one result carries; the rest stays retained and `lastSeq` stops where this output ends.
pub const RESULT_OUTPUT_MAX: usize = 32 * 1024;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WaitUntil {
    pub states: Option<Vec<MachineLifecycle>>,
    pub output: Option<String>,
    pub idle: bool,
}

#[derive(Clone, Debug)]
pub struct WaitAsk {
    pub yield_ms: u64,
    pub last_seq: Option<usize>,
    pub until: Option<WaitUntil>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitReason {
    Exited,
    State,
    Output,
    Matched,
    Idle,
    Timeout,
}

impl WaitReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exited => "exited",
            Self::State => "state",
            Self::Output => "output",
            Self::Matched => "matched",
            Self::Idle => "idle",
            Self::Timeout => "timeout",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultWindow {
    Oldest,
    Newest,
}

/// The relay host's `replay_gap` message, when `last_seq` precedes the retained output.
pub fn replay_gap(h: &LiveHandle, last_seq: Option<usize>) -> Option<String> {
    let seq = last_seq?;
    let retained_from = h.retained_from();
    if seq < retained_from {
        Some(format!(
            "last_seq {seq} precedes retained output at {retained_from} ({} bytes omitted)",
            retained_from - seq
        ))
    } else {
        None
    }
}

fn immediate(h: &LiveHandle, ask: &WaitAsk) -> Option<WaitReason> {
    let state = h.state();
    if ended(state) {
        return Some(WaitReason::Exited);
    }
    if let Some(until) = &ask.until {
        if until
            .states
            .as_ref()
            .is_some_and(|states| states.contains(&state))
        {
            return Some(WaitReason::State);
        }
        if until.idle && h.quiet() {
            return Some(WaitReason::Idle);
        }
    } else if ask.last_seq.is_some_and(|seq| seq < h.produced()) {
        return Some(WaitReason::Output);
    }
    None
}

struct StreamingUtf8 {
    pending: Vec<u8>,
}

impl StreamingUtf8 {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
        }
    }

    fn decode(&mut self, chunk: &[u8]) -> String {
        self.pending.extend_from_slice(chunk);
        let mut out = String::new();
        let mut consumed = 0usize;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(text) => {
                    out.push_str(text);
                    consumed = self.pending.len();
                }
                Err(err) => {
                    let valid_end = consumed + err.valid_up_to();
                    if let Ok(text) = std::str::from_utf8(&self.pending[consumed..valid_end]) {
                        out.push_str(text);
                    }
                    consumed = valid_end;
                    if let Some(len) = err.error_len() {
                        out.push('\u{fffd}');
                        consumed += len;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        out
    }
}

fn slice_last_chars(s: &str, max_chars: usize) -> String {
    let count = s.chars().count();
    if count <= max_chars {
        return s.to_owned();
    }
    s.chars().skip(count - max_chars).collect()
}

pub async fn wait_for_handle(
    h: &LiveHandle,
    ask: &WaitAsk,
    signal: &AbortSignal,
    after: &After,
) -> Result<WaitReason, MachineOpError> {
    // Subscribe before checking immediate state so no transition/output in between is missed.
    let mut rx = h.subscribe();
    if let Some(reason) = immediate(h, ask) {
        return Ok(reason);
    }
    if signal.is_aborted() {
        return Err(MachineOpError::new(
            OpErrorCode::Aborted,
            "the wait was cancelled",
        ));
    }

    let (timeout_tx, mut timeout_rx) = oneshot::channel::<()>();
    let cancel = (after)(
        ask.yield_ms,
        Box::new(move || {
            let _ = timeout_tx.send(());
        }),
    );

    let mut decoder = StreamingUtf8::new();
    let mut seen = String::new();

    let result = loop {
        tokio::select! {
            biased;
            _ = signal.cancelled() => {
                break Err(MachineOpError::new(OpErrorCode::Aborted, "the wait was cancelled"));
            }
            _ = &mut timeout_rx => {
                break Ok(WaitReason::Timeout);
            }
            ev = rx.recv() => {
                match ev {
                    Ok(HandleEvent::State(st)) => {
                        if ended(st) {
                            break Ok(WaitReason::Exited);
                        }
                        let matches = match &ask.until {
                            None => true,
                            Some(until) => until
                                .states
                                .as_ref()
                                .is_some_and(|states| states.contains(&st)),
                        };
                        if matches {
                            break Ok(WaitReason::State);
                        }
                    }
                    Ok(HandleEvent::Idle { .. }) => {
                        if ask.until.as_ref().is_some_and(|u| u.idle) {
                            break Ok(WaitReason::Idle);
                        }
                    }
                    Ok(HandleEvent::Output { bytes, .. }) => {
                        if let Some(target) = ask.until.as_ref().and_then(|u| u.output.as_deref()) {
                            seen.push_str(&decoder.decode(&bytes));
                            seen = slice_last_chars(&seen, WAIT_SEEN_MAX);
                            if seen.contains(target) {
                                break Ok(WaitReason::Matched);
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(reason) = immediate(h, ask) {
                            break Ok(reason);
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        if let Some(reason) = immediate(h, ask) {
                            break Ok(reason);
                        }
                        break Ok(WaitReason::Timeout);
                    }
                }
            }
        }
    };

    cancel();
    result
}

#[cfg(unix)]
fn signal_number(sig: &str) -> Option<i32> {
    let name = sig.strip_prefix("SIG").unwrap_or(sig);
    let num = match name {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "ILL" => libc::SIGILL,
        "TRAP" => libc::SIGTRAP,
        "ABRT" | "IOT" => libc::SIGABRT,
        "BUS" => libc::SIGBUS,
        "FPE" => libc::SIGFPE,
        "KILL" => libc::SIGKILL,
        "USR1" => libc::SIGUSR1,
        "SEGV" => libc::SIGSEGV,
        "USR2" => libc::SIGUSR2,
        "PIPE" => libc::SIGPIPE,
        "ALRM" => libc::SIGALRM,
        "TERM" => libc::SIGTERM,
        "CHLD" => libc::SIGCHLD,
        "CONT" => libc::SIGCONT,
        "STOP" => libc::SIGSTOP,
        "TSTP" => libc::SIGTSTP,
        "TTIN" => libc::SIGTTIN,
        "TTOU" => libc::SIGTTOU,
        "URG" => libc::SIGURG,
        "XCPU" => libc::SIGXCPU,
        "XFSZ" => libc::SIGXFSZ,
        "VTALRM" => libc::SIGVTALRM,
        "PROF" => libc::SIGPROF,
        "WINCH" => libc::SIGWINCH,
        "IO" | "POLL" => libc::SIGIO,
        "SYS" => libc::SIGSYS,
        _ => return name.parse::<i32>().ok(),
    };
    Some(num)
}

#[cfg(not(unix))]
fn signal_number(sig: &str) -> Option<i32> {
    let name = sig.strip_prefix("SIG").unwrap_or(sig);
    name.parse::<i32>().ok()
}

/// A program ended by a signal reports `128 + signal`, as a shell does.
fn exit_code_of(exit: &HandleExit) -> i32 {
    if let Some(code) = exit.exit_code {
        return code;
    }
    let Some(signal) = exit.signal.as_deref() else {
        return -1;
    };
    signal_number(signal).map_or(-1, |n| 128 + n)
}

pub fn lifecycle_of(
    state: MachineLifecycle,
    exit: Option<&HandleExit>,
    lost_reason: Option<&str>,
) -> Value {
    match state {
        MachineLifecycle::Starting | MachineLifecycle::Running | MachineLifecycle::WaitingInput => {
            json!({
                "state": state.as_str(),
                "stdinOpen": true,
                "outputClosed": false,
            })
        }
        MachineLifecycle::Exited => {
            let fallback = HandleExit {
                exit_code: None,
                signal: None,
            };
            json!({
                "state": "exited",
                "exitCode": exit_code_of(exit.unwrap_or(&fallback)),
                "stdinOpen": false,
                "outputClosed": true,
            })
        }
        MachineLifecycle::Killed => {
            json!({
                "state": "killed",
                "stdinOpen": false,
                "outputClosed": true,
            })
        }
        MachineLifecycle::Lost => {
            json!({
                "state": "lost",
                "reason": lost_reason.unwrap_or("exit_status_unavailable"),
                "stdinOpen": false,
                "outputClosed": true,
            })
        }
    }
}

/// The relay host's `TerminalResult` for a live handle, carrying at most `RESULT_OUTPUT_MAX` bytes of output.
pub fn terminal_result(
    h: &LiveHandle,
    from_seq: usize,
    now_ms: u64,
    extra: Map<String, Value>,
    window: ResultWindow,
) -> Map<String, Value> {
    let produced = h.produced();
    let retained = from_seq.max(h.retained_from()).min(produced);
    let span = h.since(retained);
    let mut skip = match window {
        ResultWindow::Newest => span.len().saturating_sub(RESULT_OUTPUT_MAX),
        ResultWindow::Oldest => 0,
    };
    let boundary_limit = skip + 3;
    while skip > 0 && skip < boundary_limit && skip < span.len() && (span[skip] & 0xc0) == 0x80 {
        skip += 1;
    }
    let from = retained + skip;
    let all = &span[skip..];
    let bytes = &all[..all.len().min(RESULT_OUTPUT_MAX)];
    let state = h.state();
    let exit = h.exit();
    let lifecycle = lifecycle_of(state, exit.as_ref(), None);

    let mut map = Map::new();
    map.insert("handleId".to_owned(), Value::String(h.id.clone()));
    map.insert(
        "output".to_owned(),
        Value::String(String::from_utf8_lossy(bytes).into_owned()),
    );
    map.insert(
        "state".to_owned(),
        Value::String(if ended(state) { "completed" } else { "running" }.to_owned()),
    );
    if lifecycle.get("state").and_then(Value::as_str) == Some("exited")
        && let Some(code) = lifecycle.get("exitCode").cloned()
    {
        map.insert("exitCode".to_owned(), code);
    }
    map.insert("lifecycle".to_owned(), lifecycle);
    map.insert("lastSeq".to_owned(), json!(from + bytes.len()));
    map.insert(
        "truncated".to_owned(),
        Value::Bool(from > from_seq || bytes.len() < all.len()),
    );
    map.insert(
        "omittedBytes".to_owned(),
        json!(from.saturating_sub(from_seq)),
    );
    map.insert("totalBytes".to_owned(), json!(produced));
    map.insert("startedAtMs".to_owned(), json!(h.started_ms));
    map.insert(
        "elapsedMs".to_owned(),
        json!(now_ms.saturating_sub(h.started_ms)),
    );
    for (k, v) in extra {
        map.insert(k, v);
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machine_op::live_handle::tokio_after;
    use maho_machine_ops::{HeldExit, HeldTerminal, OpFuture};
    use std::sync::Arc;
    use tokio::sync::{broadcast, watch};

    struct TestHeld {
        id: String,
        pid: u32,
        data_tx: broadcast::Sender<Vec<u8>>,
        exit_tx: watch::Sender<Option<HeldExit>>,
    }

    impl TestHeld {
        fn new(id: &str) -> Arc<Self> {
            let (data_tx, _) = broadcast::channel(64);
            let (exit_tx, _) = watch::channel(None);
            Arc::new(Self {
                id: id.to_owned(),
                pid: 999999,
                data_tx,
                exit_tx,
            })
        }
    }

    impl HeldTerminal for TestHeld {
        fn id(&self) -> &str {
            &self.id
        }
        fn pid(&self) -> u32 {
            self.pid
        }
        fn subscribe_data(&self) -> broadcast::Receiver<Vec<u8>> {
            self.data_tx.subscribe()
        }
        fn subscribe_exit(&self) -> watch::Receiver<Option<HeldExit>> {
            self.exit_tx.subscribe()
        }
        fn write(&self, _bytes: &[u8]) {}
        fn resize(&self, _cols: u16, _rows: u16) {}
        fn signal(&self, _signal: &str) {}
        fn screen(&self, _history: Option<usize>) -> OpFuture<'_, String> {
            Box::pin(async { Ok(String::new()) })
        }
    }

    #[tokio::test]
    async fn newest_window_skips_utf8_continuation_bytes_and_pages_forward() {
        let held = TestHeld::new("h-utf8");
        let after = tokio_after();
        let h = LiveHandle::new("h-utf8".into(), "dev".into(), held.clone(), 10, after);
        let mut rx = h.subscribe();

        // Fill more than RESULT_OUTPUT_MAX bytes ending with multi-byte UTF-8 chars.
        let mut payload = vec![b'a'; RESULT_OUTPUT_MAX + 5];
        // Put a 3-byte UTF-8 char (€ = E2 82 AC) straddling the `span.len() - RESULT_OUTPUT_MAX` boundary (index 5).
        payload[4] = 0xE2;
        payload[5] = 0x82;
        payload[6] = 0xAC;
        held.data_tx.send(payload).expect("send");
        while h.produced() < RESULT_OUTPUT_MAX + 5 {
            let _ = rx.recv().await;
        }

        let res = terminal_result(&h, 0, 20, Map::new(), ResultWindow::Newest);
        assert_eq!(res["lastSeq"], h.produced());
        assert_eq!(res["truncated"], true);
        assert_eq!(res["omittedBytes"], 7);
    }

    #[tokio::test]
    async fn wait_matches_output_and_reports_replay_gap() {
        let held = TestHeld::new("h-match");
        let after = tokio_after();
        let h = LiveHandle::new(
            "h-match".into(),
            "dev".into(),
            held.clone(),
            10,
            after.clone(),
        );

        let signal = AbortSignal::default();
        let ask = WaitAsk {
            yield_ms: 5_000,
            last_seq: None,
            until: Some(WaitUntil {
                states: None,
                output: Some("ready>".to_owned()),
                idle: false,
            }),
        };

        let h2 = Arc::clone(&h);
        let sig2 = signal.clone();
        let after2 = after.clone();
        let waiter = tokio::spawn(async move { wait_for_handle(&h2, &ask, &sig2, &after2).await });

        tokio::task::yield_now().await;
        held.data_tx.send(b"booting... ".to_vec()).expect("send1");
        held.data_tx.send(b"ready>".to_vec()).expect("send2");

        let reason = waiter.await.expect("join").expect("wait");
        assert_eq!(reason, WaitReason::Matched);
        assert_eq!(replay_gap(&h, Some(0)), None);
    }
}
