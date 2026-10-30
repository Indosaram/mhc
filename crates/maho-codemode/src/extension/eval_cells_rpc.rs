//! Port of `extension/eval-cells-rpc.ts`: session-scoped `eval_cells.list|peek|stop` extension RPC
//! handlers. They never invoke a model tool or write a session entry.

use crate::tool::{
    detached_cell_contract::EvalDetachedCellSnapshot,
    detached_cell_manager::EvalDetachedCellManager,
};
use maho_ext_api::{ExtensionApi, ExtensionFailure};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const DEFAULT_TAIL_BYTES: u64 = 8 * 1024;
const MAX_TAIL_BYTES: u64 = 64 * 1024;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

pub type EvalCellsSource =
    Arc<dyn Fn() -> Option<Arc<Mutex<EvalDetachedCellManager>>> + Send + Sync>;

fn invalid(message: &str) -> ExtensionFailure {
    ExtensionFailure::classified(
        message,
        Some("InvalidEvalCellRequestError".into()),
        Some("invalid_eval_cell_request".into()),
    )
}

fn not_found(cell_id: &str) -> ExtensionFailure {
    ExtensionFailure::classified(
        format!("Unknown detached eval cell \"{cell_id}\""),
        Some("EvalCellNotFoundError".into()),
        Some("eval_cell_not_found".into()),
    )
}

fn language(snapshot: &EvalDetachedCellSnapshot) -> &'static str {
    match snapshot.language {
        crate::tool::types::EvalLanguage::Js => "js",
    }
}

fn summary(snapshot: &EvalDetachedCellSnapshot) -> Option<&str> {
    snapshot.result.details["summary"]
        .as_str()
        .filter(|summary| !summary.is_empty())
}

/// `listEvalCells`: live cells then recent ones.
pub fn list_eval_cells(cells: &EvalDetachedCellManager) -> Value {
    let (live, recent) = cells.list();
    Value::Array(
        live.iter()
            .chain(&recent)
            .map(|snapshot| {
                let mut cell = json!({"cellId":snapshot.cell_id,"language":language(snapshot),"state":snapshot.state.as_str(),"startedAtMs":snapshot.started_at_ms});
                if let Some(queued) = &snapshot.queued_behind {
                    cell["queuedBehind"] = json!(queued);
                }
                if let Some(summary) = summary(snapshot) {
                    cell["summary"] = json!(summary);
                }
                cell
            })
            .collect(),
    )
}

/// The last `tail_bytes` (at most 64 KiB) of the output, dropping a partial leading UTF-8 character.
fn output_tail(text: &str, tail_bytes: u64) -> &str {
    let bytes = text.as_bytes();
    let keep = usize::try_from(tail_bytes.min(MAX_TAIL_BYTES)).unwrap_or(usize::MAX);
    let mut start = bytes.len().saturating_sub(keep);
    while start < bytes.len() && (bytes[start] & 0xc0) == 0x80 {
        start += 1;
    }
    &text[start..]
}

fn snapshot_reply(snapshot: &EvalDetachedCellSnapshot, tail_bytes: u64) -> Value {
    let mut reply = json!({"cellId":snapshot.cell_id,"language":language(snapshot),"state":snapshot.state.as_str(),"startedAtMs":snapshot.started_at_ms});
    if let Some(summary) = summary(snapshot) {
        reply["summary"] = json!(summary);
    }
    if let Some(queued) = &snapshot.queued_behind {
        reply["queuedBehind"] = json!(queued);
    }
    reply["outputTail"] = json!(output_tail(&snapshot.output_tail, tail_bytes));
    if let Some(retained) = snapshot.state_retained {
        reply["stateRetained"] = json!(retained);
    }
    if let Some(note) = &snapshot.interrupt_note {
        reply["interruptNote"] = json!(note);
    }
    if let Some(seconds) = snapshot.hard_limit_seconds {
        reply["hardLimitSeconds"] = json!(seconds);
    }
    if let Some(seconds) = snapshot.run_budget_seconds {
        reply["runBudgetSeconds"] = json!(seconds);
    }
    reply
}

fn parse_request(data: &Value, peek: bool) -> Result<(String, u64), ExtensionFailure> {
    let cell_id = data
        .get("cellId")
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| invalid("cellId must be a non-empty string"))?;
    let tail_bytes = match data.get("tailBytes") {
        Some(value) if peek => value
            .as_f64()
            .filter(|bytes| bytes.fract() == 0.0 && (0.0..=MAX_SAFE_INTEGER).contains(bytes))
            .map(|bytes| bytes as u64)
            .ok_or_else(|| invalid("tailBytes must be a non-negative safe integer"))?,
        _ => DEFAULT_TAIL_BYTES,
    };
    Ok((cell_id.to_owned(), tail_bytes))
}

pub fn register_eval_cells_rpc(
    api: &mut ExtensionApi,
    cells: EvalCellsSource,
) -> Result<(), ExtensionFailure> {
    let list_cells = cells.clone();
    api.rpc_handle(
        "eval_cells.list",
        Arc::new(move |_| {
            let listed = list_cells().map_or_else(
                || Value::Array(Vec::new()),
                |cells| list_eval_cells(&cells.lock().expect("cell manager")),
            );
            Box::pin(async move { Ok(listed) })
        }),
    )?;
    let peek_cells = cells.clone();
    api.rpc_handle(
        "eval_cells.peek",
        Arc::new(move |data| {
            let result = parse_request(&data, true).and_then(|(cell_id, tail_bytes)| {
                let cells = peek_cells().ok_or_else(|| not_found(&cell_id))?;
                let snapshot = cells
                    .lock()
                    .expect("cell manager")
                    .peek(&cell_id)
                    .map_err(|_| not_found(&cell_id))?;
                Ok(snapshot_reply(&snapshot, tail_bytes))
            });
            Box::pin(async move { result })
        }),
    )?;
    api.rpc_handle(
        "eval_cells.stop",
        Arc::new(move |data| {
            let request = parse_request(&data, false);
            let cells = cells();
            Box::pin(async move {
                let (cell_id, tail_bytes) = request?;
                let cells = cells.ok_or_else(|| not_found(&cell_id))?;
                let snapshot = EvalDetachedCellManager::stop(&cells, &cell_id, "stopped by rpc")
                    .await
                    .map_err(|error| {
                        if error.starts_with("Unknown detached eval cell") {
                            not_found(&cell_id)
                        } else {
                            ExtensionFailure::new(error)
                        }
                    })?;
                Ok(snapshot_reply(&snapshot, tail_bytes))
            })
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::output_tail;

    #[test]
    fn tail_drops_a_partial_leading_utf8_character() {
        assert_eq!(output_tail("ab한", 2), "");
        assert_eq!(output_tail("ab한", 3), "한");
        assert_eq!(output_tail("abc", 8 * 1024), "abc");
        let long = "x".repeat(70 * 1024);
        assert_eq!(output_tail(&long, u64::MAX).len(), 64 * 1024);
    }
}
