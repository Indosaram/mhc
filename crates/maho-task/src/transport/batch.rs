//! Parent wake batch limiting and overflow manifests (`docs/cleanroom/specs/ultrawork.md` §9.2).
use super::cap::count_utf16_code_units;
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
        let batch_id = batch_id.into();
        let overflow_count = overflow_tasks.len();
        Self {
            batch_id,
            overflow_count,
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
    if let Ok(tasks) = serde_json::from_slice::<Vec<ChildCompletion>>(&bytes) {
        let count = tasks.len();
        return Ok(WakeOverflowManifest {
            batch_id: String::new(),
            overflow_count: count,
            tasks: tasks.clone(),
            results: tasks,
        });
    }
    Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "invalid overflow manifest"))
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
        Value::Object(map)
    }
}

struct CandidateParams<'a> {
    header: Option<&'a str>,
    tasks: &'a [ChildCompletion],
    inline_count: usize,
    manifest_uri: &'a str,
}

fn render_batch_candidate(params: CandidateParams<'_>) -> String {
    let mut msg = params.header.unwrap_or("").to_owned();
    for task in params.tasks.iter().take(params.inline_count) {
        if !msg.is_empty() { msg.push('\n'); }
        msg.push_str(&task.text);
    }
    if params.inline_count < params.tasks.len() {
        let overflow_count = params.tasks.len() - params.inline_count;
        let trailer = format!("{overflow_count} more task results: {}", params.manifest_uri);
        if !msg.is_empty() { msg.push('\n'); }
        msg.push_str(&trailer);
    }
    msg
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
        });
    }
    let results_dir = options.state_dir.join("completion-results");
    let manifest_path = results_dir.join(format!("wake-overflow-{batch_id}.json"));
    let manifest_uri = format!("local://{}", manifest_path.display());

    let mut selected_inline = 0;
    for k in (0..=tasks.len()).rev() {
        let candidate = render_batch_candidate(CandidateParams {
            header: options.header,
            tasks,
            inline_count: k,
            manifest_uri: &manifest_uri,
        });
        if count_utf16_code_units(&candidate) <= limit {
            selected_inline = k;
            break;
        }
    }

    let inline_tasks = tasks[..selected_inline].to_vec();
    let overflow_tasks = tasks[selected_inline..].to_vec();
    let message = render_batch_candidate(CandidateParams {
        header: options.header,
        tasks,
        inline_count: selected_inline,
        manifest_uri: &manifest_uri,
    });

    let (overflow_manifest_path, overflow_manifest_uri) = if overflow_tasks.is_empty() {
        (None, None)
    } else {
        std::fs::create_dir_all(&results_dir)?;
        let manifest = WakeOverflowManifest::new(&batch_id, overflow_tasks.clone());
        let json_bytes = serde_json::to_vec_pretty(&manifest)?;
        std::fs::write(&manifest_path, json_bytes)?;
        (Some(manifest_path), Some(manifest_uri))
    };

    Ok(WakeBatchResult {
        batch_id,
        message,
        inline_tasks,
        overflow_tasks,
        overflow_manifest_path,
        overflow_manifest_uri,
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
    let details = if tasks.len() == 1 {
        let mut d = tasks[0].details.clone();
        if let Some(map) = d.as_object_mut() {
            map.insert("batch_id".into(), json!(res.batch_id));
        }
        d
    } else {
        res.to_details_value()
    };
    let _ = sender.send_message(
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
    );
    Ok(res)
}
