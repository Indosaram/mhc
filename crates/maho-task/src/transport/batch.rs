//! Parent wake batch limiting and overflow manifests (`docs/cleanroom/specs/ultrawork.md` §9.2).
use super::cap::{count_utf16_code_units, slice_utf16_bounded};
use maho_ext_api::{CustomMessage, DeliverAs, ExtensionApi, SendMessageOptions, ToolContent};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Default parent wake batch limit (24,000 UTF-16 code units).
pub const DEFAULT_COMPLETION_BATCH_LIMIT: usize = 24_000;
/// Minimum parent wake batch limit (2,000 UTF-16 code units).
pub const MIN_COMPLETION_BATCH_LIMIT: usize = 2_000;
/// Maximum parent wake batch limit (128,000 UTF-16 code units).
pub const MAX_COMPLETION_BATCH_LIMIT: usize = 128_000;

/// One completed child task item for batched wake notifications (§9.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildCompletion {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub response: String,
    pub text: String,
    #[serde(default)]
    pub details: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_uri: Option<String>,
}

impl ChildCompletion {
    pub fn new(id: impl Into<String>, status: impl Into<String>, text: impl Into<String>) -> Self {
        let (id, status, text) = (id.into(), status.into(), text.into());
        let details = json!({"id": &id, "status": &status, "response": &text, "text": &text});
        Self { id, status, response: text.clone(), text, details, file_uri: None }
    }
    pub fn with_response(mut self, response: impl Into<String>) -> Self {
        self.response = response.into();
        if let Some(map) = self.details.as_object_mut() {
            map.insert("response".into(), json!(self.response));
        }
        self
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }
    pub fn with_file_uri(mut self, uri: impl Into<String>) -> Self {
        let uri = uri.into();
        if let Some(map) = self.details.as_object_mut() {
            map.insert("file_uri".into(), json!(&uri));
            map.insert("uri".into(), json!(&uri));
            map.insert("file_reference".into(), json!(&uri));
        }
        self.file_uri = Some(uri);
        self
    }
}

/// Manifest file offloading overflow child results in a wake batch (§9.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeOverflowManifest {
    pub batch_id: String,
    pub overflow_count: usize,
    pub tasks: Vec<ChildCompletion>,
    pub results: Vec<ChildCompletion>,
}

impl WakeOverflowManifest {
    pub fn new(batch_id: impl Into<String>, overflow_tasks: Vec<ChildCompletion>) -> Self {
        let (batch_id, count) = (batch_id.into(), overflow_tasks.len());
        Self { batch_id, overflow_count: count, tasks: overflow_tasks.clone(), results: overflow_tasks }
    }
}

/// Reads an overflow manifest file from disk.
pub fn read_overflow_manifest(path: &Path) -> std::io::Result<WakeOverflowManifest> {
    let bytes = std::fs::read(path)?;
    if let Ok(manifest) = serde_json::from_slice::<WakeOverflowManifest>(&bytes) {
        return Ok(manifest);
    }
    serde_json::from_slice::<Vec<ChildCompletion>>(&bytes)
        .map(|tasks| WakeOverflowManifest {
            batch_id: String::new(),
            overflow_count: tasks.len(),
            results: tasks.clone(),
            tasks,
        })
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Options for building a batched wake message (§9.2).
#[derive(Clone, Copy, Debug)]
pub struct WakeBatchOptions<'a> {
    pub state_dir: &'a Path,
    pub batch_id: Option<&'a str>,
    pub limit: usize,
    pub header: Option<&'a str>,
}

impl<'a> WakeBatchOptions<'a> {
    pub fn new(state_dir: &'a Path) -> Self {
        Self { state_dir, batch_id: None, limit: DEFAULT_COMPLETION_BATCH_LIMIT, header: None }
    }
    pub fn with_batch_id(mut self, batch_id: &'a str) -> Self { self.batch_id = Some(batch_id); self }
    pub fn with_limit(mut self, limit: usize) -> Self { self.limit = limit; self }
    pub fn with_header(mut self, header: &'a str) -> Self { self.header = Some(header); self }
}

/// Outcome of building a batched parent wake message (§9.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WakeBatchResult {
    pub batch_id: String,
    pub message: String,
    pub inline_tasks: Vec<ChildCompletion>,
    pub overflow_tasks: Vec<ChildCompletion>,
    pub overflow_manifest_path: Option<PathBuf>,
    pub overflow_manifest_uri: Option<String>,
    pub manifest_error: Option<String>,
}

impl WakeBatchResult {
    pub fn to_details_value(&self) -> Value {
        let mut map = serde_json::Map::new();
        map.insert("batch_id".into(), json!(self.batch_id));
        map.insert("inline_count".into(), json!(self.inline_tasks.len()));
        map.insert("overflow_count".into(), json!(self.overflow_tasks.len()));
        if let Some(uri) = &self.overflow_manifest_uri {
            map.insert("manifest_uri".into(), json!(uri));
            map.insert("uri".into(), json!(uri));
        }
        if let Some(err) = &self.manifest_error {
            map.insert("manifest_error".into(), json!(err));
        }
        let inline_details: Vec<Value> = self.inline_tasks.iter().map(|t| {
            let mut td = t.details.clone();
            if let (Some(map), Some(uri)) = (td.as_object_mut(), &t.file_uri) {
                map.entry("file_uri".to_string()).or_insert_with(|| json!(uri));
            }
            td
        }).collect();
        map.insert("tasks".into(), json!(inline_details));
        Value::Object(map)
    }
}

fn render_batch(header: Option<&str>, inline_tasks: &[ChildCompletion], trailer: Option<&str>) -> String {
    let mut msg = header.unwrap_or("").to_owned();
    for task in inline_tasks {
        if !msg.is_empty() { msg.push('\n'); }
        msg.push_str(&task.text);
    }
    if let Some(t) = trailer {
        if !msg.is_empty() { msg.push('\n'); }
        msg.push_str(t);
    }
    msg
}

/// The largest `k <= candidates.len()` whose message (the first `k` candidates plus
/// `trailer(k)`) fits `limit`; `trailer(k)` describes every task left out of an inline set of `k`.
fn select_inline(
    header: Option<&str>,
    candidates: &[ChildCompletion],
    limit: usize,
    trailer: &dyn Fn(usize) -> Option<String>,
) -> usize {
    (0..=candidates.len())
        .rev()
        .find(|&k| {
            let text = trailer(k);
            count_utf16_code_units(&render_batch(header, &candidates[..k], text.as_deref())) <= limit
        })
        .unwrap_or(0)
}

/// Builds a batched parent wake message according to §9.2.
pub fn build_wake_batch(
    options: WakeBatchOptions<'_>,
    tasks: &[ChildCompletion],
) -> std::io::Result<WakeBatchResult> {
    let limit = options.limit.clamp(MIN_COMPLETION_BATCH_LIMIT, MAX_COMPLETION_BATCH_LIMIT);
    let batch_id = options.batch_id.map(str::to_owned).unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if tasks.is_empty() {
        return Ok(WakeBatchResult {
            batch_id,
            message: options.header.unwrap_or("").to_owned(),
            inline_tasks: Vec::new(),
            overflow_tasks: Vec::new(),
            overflow_manifest_path: None,
            overflow_manifest_uri: None,
            manifest_error: None,
        });
    }
    let results_dir = options.state_dir.join("completion-results");
    let manifest_path = results_dir.join(format!("wake-overflow-{batch_id}.json"));
    let manifest_uri = format!("local://{}", manifest_path.display());

    let total = tasks.len();
    let uri_trailer =
        |k: usize| (k < total).then(|| format!("{} more task results: {manifest_uri}", total - k));
    let mut selected_inline = select_inline(options.header, tasks, limit, &uri_trailer);

    let (overflow_manifest_path, overflow_manifest_uri, manifest_error) = if selected_inline == total {
        (None, None, None)
    } else {
        let write_res = std::fs::create_dir_all(&results_dir).and_then(|()| {
            let manifest = WakeOverflowManifest::new(&batch_id, tasks[selected_inline..].to_vec());
            let bytes = serde_json::to_vec_pretty(&manifest).map_err(std::io::Error::other)?;
            std::fs::write(&manifest_path, bytes)?;
            Ok((manifest_path, manifest_uri.clone()))
        });
        match write_res {
            Ok((path, uri)) => (Some(path), Some(uri), None),
            Err(e) => (None, None, Some(e.to_string())),
        }
    };

    let trailer = match &manifest_error {
        // The error trailer can be longer than the pointer it replaces: choose the inline set
        // again so the message stays within the limit.
        Some(err) => {
            let error_trailer = |k: usize| {
                Some(format!("{} more task results could not be saved: {err}", total - k))
            };
            selected_inline =
                select_inline(options.header, &tasks[..selected_inline], limit, &error_trailer);
            error_trailer(selected_inline)
        }
        None => uri_trailer(selected_inline),
    };
    let inline_tasks = tasks[..selected_inline].to_vec();
    let overflow_tasks = tasks[selected_inline..].to_vec();
    let rendered = render_batch(options.header, &inline_tasks, trailer.as_deref());
    // Only a header or trailer longer than the limit by itself reaches the slice.
    let message = slice_utf16_bounded(&rendered, limit).0.to_owned();

    Ok(WakeBatchResult {
        batch_id,
        message,
        inline_tasks,
        overflow_tasks,
        overflow_manifest_path,
        overflow_manifest_uri,
        manifest_error,
    })
}

/// Delivers a batch of child completions to the parent session, enforcing `batch_limit`.
pub fn deliver_batch(
    sender: &ExtensionApi,
    state_dir: &Path,
    batch_limit: usize,
    tasks: &[ChildCompletion],
) -> std::io::Result<WakeBatchResult> {
    let res = build_wake_batch(
        WakeBatchOptions::new(state_dir).with_limit(batch_limit),
        tasks,
    )?;
    // A single task keeps its own details only when it went inline; an overflowed task's full
    // response must not ride along in the details of a bounded wake.
    let details = if let ([task], []) = (res.inline_tasks.as_slice(), res.overflow_tasks.as_slice()) {
        let mut d = task.details.clone();
        if let Some(map) = d.as_object_mut() {
            map.insert("batch_id".into(), json!(res.batch_id));
        }
        d
    } else {
        res.to_details_value()
    };
    sender
        .send_message(
            CustomMessage {
                custom_type: crate::extension::RESULT_MESSAGE_TYPE.to_owned(),
                content: vec![ToolContent::text(&res.message)],
                display: true,
                details: Some(details),
            },
            SendMessageOptions {
                trigger_turn: true,
                deliver_as: Some(DeliverAs::FollowUp),
            },
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(res)
}
