//! `runners/input-admission-read.ts`: the child's admission of each input id (`get_pending_inputs`).

use serde_json::{Value, json};

use crate::runners::input_effects::{InputAdmission, InputAdmissions};
use crate::runners::rpc::protocol_client::RpcClientPort;
use crate::runners::rpc_host::handle_port::HostSessionPort;

/// `get_pending_inputs` answers at most this many `knownClientMessageIds` per read (senpi#3549).
pub const MAX_IDS_PER_READ: usize = 64;

/// One batch's outcomes, or `None` when the child has no such read at all.
type BatchAnswer = Option<Vec<(String, InputAdmission)>>;

/// The child's admission of each id, read through its daemon session. A port without the read
/// answers `Ok(None)` (the caller keeps its same-id resend); a failed read is an error, so the input
/// stays uncertain.
pub fn read_host_admissions(
    port: &dyn HostSessionPort,
    ids: &[String],
) -> Result<Option<InputAdmissions>, String> {
    collect(ids, |batch| {
        match port
            .get_pending_inputs(batch)
            .map_err(|error| error.message)?
        {
            None => Ok(None),
            Some(answer) => outcomes(&answer).map(Some),
        }
    })
}

/// The same read over a stdio child. Only an "unknown command" refusal means the child cannot read
/// admissions (an engine before senpi#3549); any other refusal or a lost answer is an error.
pub fn read_stdio_admissions(
    client: &dyn RpcClientPort,
    ids: &[String],
) -> Result<Option<InputAdmissions>, String> {
    collect(ids, |batch| {
        let response = client
            .send(json!({ "type": "get_pending_inputs", "knownClientMessageIds": batch }))
            .map_err(|error| error.to_string())?;
        let success = response.get("success").and_then(Value::as_bool) == Some(true);
        if success && response.get("command").and_then(Value::as_str) == Some("get_pending_inputs")
        {
            return outcomes(&response["data"]).map(Some);
        }
        let error = response.get("error").and_then(Value::as_str).unwrap_or("");
        if !success && error.to_lowercase().contains("unknown command") {
            return Ok(None);
        }
        let detail = if success {
            response
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or("")
        } else {
            error
        };
        Err(format!("get_pending_inputs refused: {detail}"))
    })
}

fn outcomes(answer: &Value) -> Result<Vec<(String, InputAdmission)>, String> {
    answer
        .get("outcomes")
        .and_then(Value::as_array)
        .ok_or("get_pending_inputs answered without outcomes")?
        .iter()
        .map(|outcome| {
            let id = outcome
                .get("clientMessageId")
                .and_then(Value::as_str)
                .ok_or("get_pending_inputs outcome without an id")?;
            let status = match outcome.get("status").and_then(Value::as_str) {
                Some("admitted") => InputAdmission::Admitted,
                Some("not_admitted") => InputAdmission::NotAdmitted,
                _ => return Err(format!("get_pending_inputs outcome for {id} has no status")),
            };
            Ok((id.to_string(), status))
        })
        .collect()
}

fn collect(
    ids: &[String],
    mut read_batch: impl FnMut(&[String]) -> Result<BatchAnswer, String>,
) -> Result<Option<InputAdmissions>, String> {
    let mut admissions = InputAdmissions::new();
    for batch in ids.chunks(MAX_IDS_PER_READ) {
        let Some(answer) = read_batch(batch)? else {
            return Ok(None);
        };
        admissions.extend(answer);
    }
    // The engine answers every id it is asked about. An id left out is never read as not admitted.
    let missing = ids
        .iter()
        .filter(|id| !admissions.contains_key(*id))
        .count();
    if missing > 0 {
        return Err(format!("get_pending_inputs left out {missing} id(s)"));
    }
    Ok(Some(admissions))
}

#[cfg(test)]
#[path = "input_admission_read_tests.rs"]
mod tests;
