//! Port of `packages/coding-agent/src/modes/rpc/machine-op/terminal-ops.ts`.
//!
//! The run step of `exec_command`, `write_stdin`, `wait_for_command` and `kill_terminal`.

use crate::machine_op::live_handle::{After, ENDED, HandleExit, ended};
use crate::machine_op::machine_terminals::{MachineTerminals, SpawnAsk};
use crate::machine_op::real_path::spawn_path_in;
use crate::machine_op::terminal_args::{ExecArgs, KillArgs, WaitArgs, WriteStdinArgs};
use crate::machine_op::terminal_input::{InputQueues, StdinInput};
use crate::machine_op::terminal_wait::{
    ResultWindow, WaitAsk, WaitUntil, lifecycle_of, replay_gap, terminal_result, wait_for_handle,
};
use crate::machine_op::wire::OpOutcome;
use maho_machine_ops::{AbortSignal, MachineOpError};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

pub struct TerminalRun<'a> {
    pub sender: &'a str,
    pub root: &'a str,
    pub confined: bool,
    pub signal: &'a AbortSignal,
    pub env_overlay: &'a BTreeMap<String, String>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn fail(code: &str, message: impl Into<String>) -> OpOutcome {
    OpOutcome::failed(code, message)
}

fn applied(response: Map<String, Value>) -> OpOutcome {
    OpOutcome::applied(response)
}

pub struct TerminalOps {
    terminals: Arc<MachineTerminals>,
    input: InputQueues,
    after: After,
}

impl TerminalOps {
    pub fn new(terminals: Arc<MachineTerminals>, after: After) -> Self {
        let input = InputQueues::new(Arc::clone(&after));
        Self {
            terminals,
            input,
            after,
        }
    }

    /// A handle no longer in memory answers from its summary row, as the relay host answers from its handle table.
    fn gone(&self, id: &str) -> OpOutcome {
        let Some(r) = self.terminals.record(id) else {
            return fail("unknown_handle", format!("no handle {id} on this host"));
        };
        let exit = HandleExit {
            exit_code: r.exit_code,
            signal: None,
        };
        let mut map = Map::new();
        map.insert("handleId".to_owned(), Value::String(id.to_owned()));
        map.insert("output".to_owned(), Value::String(String::new()));
        map.insert(
            "state".to_owned(),
            Value::String(
                if ended(r.state) {
                    "completed"
                } else {
                    "running"
                }
                .to_owned(),
            ),
        );
        map.insert(
            "lifecycle".to_owned(),
            lifecycle_of(r.state, Some(&exit), Some("agent_restarted")),
        );
        map.insert("lastSeq".to_owned(), json!(0));
        map.insert("truncated".to_owned(), Value::Bool(false));
        map.insert("omittedBytes".to_owned(), json!(0));
        map.insert("totalBytes".to_owned(), json!(0));
        if let Some(code) = r.exit_code {
            map.insert("exitCode".to_owned(), json!(code));
        }
        map.insert("startedAtMs".to_owned(), json!(r.started_ms));
        map.insert("reason".to_owned(), Value::String("exited".to_owned()));
        applied(map)
    }

    pub async fn exec_command(
        &self,
        d: ExecArgs,
        call: &TerminalRun<'_>,
    ) -> Result<OpOutcome, MachineOpError> {
        let cwd = match &d.cwd {
            None => call.root.to_owned(),
            Some(raw) => spawn_path_in(call.root, raw, call.confined)
                .await?
                .to_string_lossy()
                .into_owned(),
        };
        let h = self
            .terminals
            .spawn(SpawnAsk {
                sender: call.sender.to_owned(),
                command: d.command,
                cwd,
                env: call.env_overlay.clone(),
                cols: d.cols,
                rows: d.rows,
            })
            .await?;

        // Review M2: an initial wait aborted (deadline, cancel, shutdown) returns no handle, so nothing keeps running.
        let wait_ask = WaitAsk {
            yield_ms: d.yield_ms,
            last_seq: None,
            until: Some(WaitUntil {
                states: Some(ENDED.to_vec()),
                output: None,
                idle: false,
            }),
        };
        let reason = match wait_for_handle(&h, &wait_ask, call.signal, &self.after).await {
            Ok(reason) => reason,
            Err(err) => {
                self.terminals.kill(&h).await;
                return Err(err);
            }
        };

        let mut extra = Map::new();
        extra.insert("pid".to_owned(), json!(h.pid));
        extra.insert(
            "reason".to_owned(),
            Value::String(reason.as_str().to_owned()),
        );
        Ok(applied(terminal_result(
            &h,
            0,
            now_ms(),
            extra,
            ResultWindow::Oldest,
        )))
    }

    pub async fn write_stdin(
        &self,
        d: WriteStdinArgs,
        call: &TerminalRun<'_>,
    ) -> Result<OpOutcome, MachineOpError> {
        let Some(h) = self.terminals.get(&d.handle) else {
            return Ok(self.gone(&d.handle));
        };
        if ended(h.state()) {
            return Ok(fail(
                "stdin_closed",
                format!("handle {} has ended", d.handle),
            ));
        }
        let before = h.produced();
        let effect = self
            .input
            .write(
                &h,
                StdinInput {
                    data: d.data.map(|s| s.into_bytes()),
                    keys: d.keys,
                    paste: d.paste,
                },
                d.expect_ms,
            )
            .await;
        let reason = wait_for_handle(
            &h,
            &WaitAsk {
                yield_ms: d.yield_ms,
                last_seq: None,
                until: None,
            },
            call.signal,
            &self.after,
        )
        .await?;
        let observed = effect.observed.unwrap_or_else(|| h.produced() > before);
        let mut extra = Map::new();
        extra.insert(
            "stdinEffect".to_owned(),
            json!({
                "acceptedBytes": effect.accepted_bytes,
                "observed": observed,
            }),
        );
        extra.insert(
            "reason".to_owned(),
            Value::String(reason.as_str().to_owned()),
        );
        Ok(applied(terminal_result(
            &h,
            d.last_seq.unwrap_or(before),
            now_ms(),
            extra,
            ResultWindow::Oldest,
        )))
    }

    pub async fn wait_for_command(
        &self,
        d: WaitArgs,
        call: &TerminalRun<'_>,
    ) -> Result<OpOutcome, MachineOpError> {
        let Some(h) = self.terminals.get(&d.handle) else {
            return Ok(self.gone(&d.handle));
        };
        if let Some(gap) = replay_gap(&h, d.last_seq) {
            return Ok(fail("replay_gap", gap));
        }
        let reason = wait_for_handle(
            &h,
            &WaitAsk {
                yield_ms: d.yield_ms,
                last_seq: d.last_seq,
                until: d.until,
            },
            call.signal,
            &self.after,
        )
        .await?;
        let window = if d.last_seq.is_none() {
            ResultWindow::Newest
        } else {
            ResultWindow::Oldest
        };
        let mut extra = Map::new();
        extra.insert(
            "reason".to_owned(),
            Value::String(reason.as_str().to_owned()),
        );
        Ok(applied(terminal_result(
            &h,
            d.last_seq.unwrap_or(0),
            now_ms(),
            extra,
            window,
        )))
    }

    pub async fn kill_terminal(&self, d: KillArgs) -> OpOutcome {
        let Some(h) = self.terminals.get(&d.handle) else {
            let Some(r) = self.terminals.record(&d.handle) else {
                return fail(
                    "unknown_handle",
                    format!("no handle {} on this host", d.handle),
                );
            };
            let exit = HandleExit {
                exit_code: r.exit_code,
                signal: None,
            };
            let mut map = Map::new();
            map.insert("handleId".to_owned(), Value::String(d.handle));
            map.insert("state".to_owned(), Value::String("task-failed".to_owned()));
            map.insert("reason".to_owned(), Value::String("killed".to_owned()));
            map.insert(
                "lifecycle".to_owned(),
                lifecycle_of(r.state, Some(&exit), Some("agent_restarted")),
            );
            return applied(map);
        };
        self.terminals.kill(&h).await;
        let mut map = Map::new();
        map.insert("handleId".to_owned(), Value::String(h.id.clone()));
        map.insert("state".to_owned(), Value::String("task-failed".to_owned()));
        map.insert("reason".to_owned(), Value::String("killed".to_owned()));
        map.insert(
            "lifecycle".to_owned(),
            lifecycle_of(h.state(), h.exit().as_ref(), None),
        );
        applied(map)
    }
}
