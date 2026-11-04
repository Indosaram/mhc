//! Per-child completion response transport limits (`docs/cleanroom/specs/ultrawork.md` §9.1).
use crate::record::TaskRecord;
use maho_ext_api::ExtensionApi;
use serde_json::{Value, json};
use std::path::Path;

/// Default per-child completion response limit (8,000 UTF-16 code units).
pub const DEFAULT_COMPLETION_TRANSPORT_LIMIT: usize = 8_000;
/// Minimum per-child completion response limit (1,000 UTF-16 code units).
pub const MIN_COMPLETION_TRANSPORT_LIMIT: usize = 1_000;
/// Maximum per-child completion response limit (64,000 UTF-16 code units).
pub const MAX_COMPLETION_TRANSPORT_LIMIT: usize = 64_000;

/// Counts the number of UTF-16 code units in `s`.
pub fn count_utf16_code_units(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Slices `s` up to `limit` UTF-16 code units without breaking Unicode scalar values
/// (surrogate pairs in UTF-16 are never split). Returns `(slice, units_count)`.
pub fn slice_utf16_bounded(s: &str, limit: usize) -> (&str, usize) {
    let mut units = 0;
    let mut byte_end = 0;
    for (idx, ch) in s.char_indices() {
        let ch_units = ch.len_utf16();
        if units + ch_units > limit {
            break;
        }
        units += ch_units;
        byte_end = idx + ch.len_utf8();
    }
    (&s[..byte_end], units)
}

/// Options for capping a child task's completion response (§9.1).
#[derive(Clone, Copy, Debug)]
pub struct CompletionCapOptions<'a> {
    pub state_dir: &'a Path,
    pub task_id: &'a str,
    pub status: &'a str,
    pub response: &'a str,
    pub limit: usize,
}

impl<'a> CompletionCapOptions<'a> {
    pub fn new(state_dir: &'a Path, task_id: &'a str, response: &'a str) -> Self {
        Self {
            state_dir,
            task_id,
            status: "completed",
            response,
            limit: DEFAULT_COMPLETION_TRANSPORT_LIMIT,
        }
    }
    pub fn with_status(mut self, status: &'a str) -> Self {
        self.status = status;
        self
    }
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
}

/// Caps a child task's raw response according to §9.1.
///
/// If response length in UTF-16 code units <= limit, returns details with the inline response.
/// If response length > limit, writes unmodified text to
/// `<state_dir>/completion-results/<task_id>.txt`, cleanly slices at the limit without splitting
/// surrogate pairs, and returns details with sliced text and the visible pointer line.
pub fn cap_completion_response(options: CompletionCapOptions<'_>) -> std::io::Result<Value> {
    let limit = options
        .limit
        .clamp(MIN_COMPLETION_TRANSPORT_LIMIT, MAX_COMPLETION_TRANSPORT_LIMIT);
    let total_units = count_utf16_code_units(options.response);
    if total_units <= limit {
        return Ok(json!({
            "id": options.task_id,
            "status": options.status,
            "response": options.response,
            "text": options.response,
            "body": options.response,
            "spilled": false,
        }));
    }
    let (sliced, _) = slice_utf16_bounded(options.response, limit);
    let results_dir = options.state_dir.join("completion-results");
    let write_res = std::fs::create_dir_all(&results_dir).and_then(|_| {
        let file_path = results_dir.join(format!("{}.txt", options.task_id));
        std::fs::write(&file_path, options.response)?;
        Ok(file_path)
    });

    match write_res {
        Ok(file_path) => {
            let uri = format!("local://{}", file_path.display());
            let abs_path = file_path.display().to_string();
            let pointer = format!("\n\nFull result: {uri} ({abs_path})");
            let body = format!("{sliced}{pointer}");
            Ok(json!({
                "id": options.task_id,
                "status": options.status,
                "response": sliced,
                "text": sliced,
                "body": body,
                "spilled": true,
                "file_uri": &uri,
                "file_path": &abs_path,
                "uri": &uri,
                "file_reference": &uri,
            }))
        }
        Err(error) => {
            let note = format!("\n\nfull result could not be saved: {error}");
            let body = format!("{sliced}{note}");
            Ok(json!({
                "id": options.task_id,
                "status": options.status,
                "response": sliced,
                "text": sliced,
                "body": body,
                "spilled": false,
                "error": error.to_string(),
            }))
        }
    }
}

/// Caps a child task's raw response and formats a well-formed `<task-result>` XML envelope.
pub fn build_capped_result_envelope(
    state_dir: &Path,
    record: &TaskRecord,
    raw_response: &str,
    transport_limit: usize,
) -> (String, Value) {
    let options = CompletionCapOptions::new(state_dir, &record.id, raw_response)
        .with_status(record.status.as_str())
        .with_limit(transport_limit);
    let details = cap_completion_response(options).unwrap_or_else(|error| {
        let limit = transport_limit.clamp(
            MIN_COMPLETION_TRANSPORT_LIMIT,
            MAX_COMPLETION_TRANSPORT_LIMIT,
        );
        let (sliced, _) = slice_utf16_bounded(raw_response, limit);
        let body = format!("{sliced}\n\nfull result could not be saved: {error}");
        json!({
            "id": record.id,
            "status": record.status.as_str(),
            "response": sliced,
            "text": sliced,
            "body": body,
            "spilled": false,
            "error": error.to_string(),
        })
    });
    let body = details
        .get("body")
        .and_then(Value::as_str)
        .unwrap_or(raw_response);
    let envelope = crate::summary::format_result_envelope(record, body);
    (envelope, details)
}

/// Delivery options for a settled task.
pub struct SettledDelivery<'a> {
    pub sender: &'a ExtensionApi,
    pub state_dir: &'a Path,
    pub record: &'a TaskRecord,
    pub raw_response: &'a str,
    pub limit: usize,
}

/// Delivers a settled background task result with transport capping applied.
pub fn deliver_settled(delivery: SettledDelivery<'_>) -> std::io::Result<()> {
    let raw = if !delivery.raw_response.is_empty() {
        delivery.raw_response.to_owned()
    } else {
        let state = crate::record::StateDir::new(delivery.state_dir);
        let disk_out = state.read_output(&delivery.record.id);
        if !disk_out.trim().is_empty() {
            disk_out
        } else {
            state.read_log_tail(&delivery.record.id, 4_000)
        }
    };
    let (envelope, details) = build_capped_result_envelope(
        delivery.state_dir,
        delivery.record,
        &raw,
        delivery.limit,
    );
    let completion = super::batch::ChildCompletion::new(
        &delivery.record.id,
        delivery.record.status.as_str(),
        envelope,
    )
    .with_details(details);
    let _ = super::batch::deliver_batch(
        delivery.sender,
        delivery.state_dir,
        super::batch::DEFAULT_COMPLETION_BATCH_LIMIT,
        &[completion],
    )?;
    Ok(())
}
