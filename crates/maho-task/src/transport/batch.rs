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
        let details = json!({"id": &id, "status": &status});
        Self {
            id,
            status,
            response: text.clone(),
            text,
            details,
            file_uri: None,
        }
    }
    pub fn with_response(mut self, response: impl Into<String>) -> Self {
        self.response = response.into();
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
        Self {
            batch_id,
            overflow_count: count,
            tasks: overflow_tasks.clone(),
            results: overflow_tasks,
        }
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
        Self {
            state_dir,
            batch_id: None,
            limit: DEFAULT_COMPLETION_BATCH_LIMIT,
            header: None,
        }
    }
    pub fn with_batch_id(mut self, batch_id: &'a str) -> Self {
        self.batch_id = Some(batch_id);
        self
    }
    pub fn with_limit(mut self, limit: usize) -> Self {
        self.limit = limit;
        self
    }
    pub fn with_header(mut self, header: &'a str) -> Self {
        self.header = Some(header);
        self
    }
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
    /// Wake details: batch counters, the manifest pointer, and `{task_id, status, file_uri,
    /// truncated}` per inline task. Task text lives only in the message content.
    pub fn to_details_value(&self) -> Value {
        wake_details(
            &self.inline_tasks,
            WakeFacts {
                batch_id: &self.batch_id,
                overflow_count: self.overflow_tasks.len(),
                manifest_uri: self.overflow_manifest_uri.as_deref(),
                manifest_error: self.manifest_error.as_deref(),
            },
        )
    }
}

struct WakeFacts<'a> {
    batch_id: &'a str,
    overflow_count: usize,
    manifest_uri: Option<&'a str>,
    manifest_error: Option<&'a str>,
}

fn task_entry(task: &ChildCompletion) -> Value {
    let uri = task
        .file_uri
        .as_deref()
        .or_else(|| task.details.get("file_uri").and_then(Value::as_str));
    let truncated = task.details.get("spilled").and_then(Value::as_bool) == Some(true)
        || task.details.get("error").is_some();
    json!({"task_id": task.id, "status": task.status, "file_uri": uri, "truncated": truncated})
}

fn wake_details(inline: &[ChildCompletion], facts: WakeFacts<'_>) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("batch_id".into(), json!(facts.batch_id));
    map.insert("inline_count".into(), json!(inline.len()));
    map.insert("overflow_count".into(), json!(facts.overflow_count));
    if let Some(uri) = facts.manifest_uri {
        map.insert("manifest_uri".into(), json!(uri));
    }
    if let Some(err) = facts.manifest_error {
        map.insert("manifest_error".into(), json!(err));
    }
    map.insert(
        "tasks".into(),
        Value::Array(inline.iter().map(task_entry).collect()),
    );
    Value::Object(map)
}

fn render_batch(
    header: Option<&str>,
    inline_tasks: &[ChildCompletion],
    trailer: Option<&str>,
) -> String {
    let mut msg = header.unwrap_or("").to_owned();
    for task in inline_tasks {
        if !msg.is_empty() {
            msg.push('\n');
        }
        msg.push_str(&task.text);
    }
    if let Some(t) = trailer {
        if !msg.is_empty() {
            msg.push('\n');
        }
        msg.push_str(t);
    }
    msg
}

/// What a wake with `k` inline tasks says about the tasks left out: the trailer line and the
/// manifest facts that go into the details.
struct Plan {
    trailer: Option<String>,
    manifest_uri: Option<String>,
    manifest_error: Option<String>,
}

struct Budget<'a> {
    header: Option<&'a str>,
    batch_id: &'a str,
    limit: usize,
    total: usize,
}

/// The largest `k <= candidates.len()` whose wake fits `limit`: the rendered message (the first
/// `k` candidates plus the plan's trailer) plus the serialized details, both in UTF-16 units.
fn select_inline(
    candidates: &[ChildCompletion],
    budget: &Budget<'_>,
    plan: &dyn Fn(usize) -> Plan,
) -> usize {
    (0..=candidates.len())
        .rev()
        .find(|&k| {
            let plan = plan(k);
            let content = render_batch(budget.header, &candidates[..k], plan.trailer.as_deref());
            let details = wake_details(
                &candidates[..k],
                WakeFacts {
                    batch_id: budget.batch_id,
                    overflow_count: budget.total - k,
                    manifest_uri: plan.manifest_uri.as_deref(),
                    manifest_error: plan.manifest_error.as_deref(),
                },
            );
            count_utf16_code_units(&content) + count_utf16_code_units(&details.to_string())
                <= budget.limit
        })
        .unwrap_or(0)
}

/// Builds a batched parent wake message according to §9.2.
pub fn build_wake_batch(
    options: WakeBatchOptions<'_>,
    tasks: &[ChildCompletion],
) -> std::io::Result<WakeBatchResult> {
    let limit = options
        .limit
        .clamp(MIN_COMPLETION_BATCH_LIMIT, MAX_COMPLETION_BATCH_LIMIT);
    let batch_id = options
        .batch_id
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
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
    let budget = Budget {
        header: options.header,
        batch_id: &batch_id,
        limit,
        total,
    };
    let uri_plan = |k: usize| {
        let pointing = k < total;
        Plan {
            trailer: pointing.then(|| format!("{} more task results: {manifest_uri}", total - k)),
            manifest_uri: pointing.then(|| manifest_uri.clone()),
            manifest_error: None,
        }
    };
    let mut selected_inline = select_inline(tasks, &budget, &uri_plan);

    let (overflow_manifest_path, overflow_manifest_uri, manifest_error) = if selected_inline
        == total
    {
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
            let error_plan = |k: usize| Plan {
                trailer: Some(format!(
                    "{} more task results could not be saved: {err}",
                    total - k
                )),
                manifest_uri: None,
                manifest_error: Some(err.clone()),
            };
            selected_inline = select_inline(&tasks[..selected_inline], &budget, &error_plan);
            error_plan(selected_inline).trailer
        }
        None => uri_plan(selected_inline).trailer,
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
    sender
        .send_message(
            CustomMessage {
                custom_type: crate::extension::RESULT_MESSAGE_TYPE.to_owned(),
                content: vec![ToolContent::text(&res.message)],
                display: true,
                details: Some(res.to_details_value()),
            },
            SendMessageOptions {
                trigger_turn: true,
                deliver_as: Some(DeliverAs::FollowUp),
            },
        )
        .map_err(|error| std::io::Error::other(error.to_string()))?;
    Ok(res)
}
