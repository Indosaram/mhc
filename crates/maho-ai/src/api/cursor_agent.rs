//! Port of senpi packages/ai/src/api/cursor-agent.ts.
// ported by todo 12
//!
//! Cursor agent protocol (`agent.v1.AgentService/Run`) over HTTP/2 Connect.
//! The protocol is server-driven: the client opens one `Run` stream per
//! assistant turn, the server streams interaction updates (text/thinking/
//! tool-call deltas) and, mid-turn, sends `ExecServerMessage` frames asking the
//! CLIENT to execute a tool and blocks until the answer arrives on the same
//! stream. Tool execution is therefore bridged through injected
//! [`CursorExecHandlers`]; each bridged call is synthesized into the assistant
//! message as an already-resolved `toolCall` block (marked cursor-exec-resolved
//! so the agent loop never re-runs it) and paired with a `ToolResultMessage`
//! delivered via `onToolResult`.

pub mod deterministic_id;
pub mod exec_lifecycle;
pub mod exec_modern;
pub mod r#gen;
pub mod measure;
pub mod pi_args;
pub mod reasoning_params;
pub mod stream_retry;
pub mod types;
pub mod value_pb;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use prost::Message as _;
use serde_json::{json, Map, Value};

use crate::cursor::composer_prompt::{is_cursor_composer_model, CURSOR_COMPOSER_PROMPT};
use crate::models::calculate_cost;
use crate::session_resources::register_session_resource_cleanup;
use crate::types::{
    AssistantMessage, AssistantMessageEvent, ContentBlock, Context, DoneReason, ErrorReason, Message, Model,
    SimpleStreamOptions, StreamOptions, TextContent, Tool, ToolCall, ToolResultMessage, Usage,
};
use crate::utils::abort::AbortSignal;
use crate::utils::block_symbols::StreamingBlockState;
use crate::utils::cursor_context_limit::record_cursor_context_limit;
use crate::utils::diagnostics::now_ms;
use crate::utils::event_stream::AssistantMessageEventStream;
use crate::utils::headers::provider_headers_to_record;
use crate::utils::json_parse::{parse_json_with_repair, parse_streaming_json};
use crate::utils::sanitize_unicode::sanitize_surrogates;
use crate::utils::uuid::uuidv7;

fn random_uuid() -> String {
    uuidv7(None).unwrap_or_else(|_| uuid::Uuid::new_v4().to_string())
}

use super::cursor_agent::exec_modern::{
    build_mcp_state_result, build_neutral_hook_result, build_pi_bash_error, build_pi_bash_result, build_pi_edit_error,
    build_pi_edit_rejected, build_pi_edit_result, build_pi_find_error, build_pi_find_result, build_pi_grep_error,
    build_pi_grep_result, build_pi_ls_error, build_pi_ls_result, build_pi_read_error, build_pi_read_result,
    build_pi_write_error, build_pi_write_rejected, build_pi_write_result,
};
use super::cursor_agent::r#gen::agent_pb::{
    agent_client_message, agent_server_message, conversation_action, conversation_step, conversation_turn_structure,
    exec_client_control_message, exec_client_message, exec_server_message, interaction_update, kv_client_message,
    kv_server_message, mcp_result, mcp_tool_result, mcp_tool_result_content_item, read_result, read_success,
    shell_result, shell_stream, tool_call, AgentClientMessage, AgentConversationTurnStructure, AgentRunRequest,
    AgentServerMessage, AgentStoreConflictError, AgentStoreConflictResult, BackgroundShellSpawnResult,
    CanvasDiagnosticsError, CanvasDiagnosticsResult, ClientHeartbeat, ComputerUseError, ComputerUseResult,
    ConversationAction, ConversationSearchError, ConversationSearchResult, ConversationStateStructure,
    ConversationStep, ConversationTurnStructure, DeleteError, DeleteRejected, DeleteResult, DeleteSuccess,
    DiagnosticsError, DiagnosticsRejected, DiagnosticsResult, DiagnosticsSuccess, ExecClientControlMessage,
    ExecClientHeartbeat, ExecClientMessage, ExecClientStreamClose, ExecClientThrow, ExecServerMessage, FetchError,
    FetchResult, ForceBackgroundShellResult, ForceBackgroundShellStatus, ForceBackgroundSubagentResult,
    ForceBackgroundSubagentStatus, GetBlobResult, GetUsableModelsRequest, GetUsableModelsResponse, GrepContentMatch,
    GrepContentResult, GrepCountResult, GrepError, GrepFileCount, GrepFileMatch, GrepFilesResult, GrepResult,
    GrepSuccess, GrepUnionResult, KvClientMessage, KvServerMessage, ListMcpResourcesExecResult,
    ListMcpResourcesSuccess, LsDirectoryTreeNode, LsDirectoryTreeNodeFile, LsError, LsRejected, LsResult, LsSuccess,
    McpAllowlistPrecheckResult, McpApproved, McpArgs, McpError, McpImageContent, McpRejected, McpResult, McpSuccess,
    McpTextContent, McpToolCall, McpToolDefinition, McpToolError, McpToolNotFound, McpToolResult,
    McpToolResultContentItem, ModelDetails, ReadError, ReadMcpResourceExecResult, ReadMcpResourceNotFound,
    ReadRejected, ReadResult, ReadSuccess, RecordScreenFailure, RecordScreenResult, RequestContext,
    RequestContextResult, RequestContextSuccess, RequestedModel, ResumeAction, SelectedContext, SelectedImage,
    SetBlobResult, ShellAllowlistPrecheckResult, ShellArgs, ShellFailure, ShellRejected, ShellResult, ShellStream,
    ShellStreamExit, ShellStreamStderr, ShellStreamStdout, ShellSuccess, SmartModeClassifierError,
    SmartModeClassifierResult, SubagentAwaitNotFound, SubagentAwaitResult, SubagentError, SubagentResult, ToolCall as PbToolCall,
    TurnEndedUpdate, UserMessage as PbUserMessage, UserMessageAction, WebFetchAllowlistPrecheckResult, WriteError,
    WriteRejected, WriteResult, WriteShellStdinError, WriteShellStdinResult, WriteSuccess,
};
use super::cursor_agent::r#gen::agent_pb::*;
use super::cursor_agent::pi_args::{compose_shell_command, omit_undefined_args, pi_limit, pi_ls_path, pi_read_args, pi_timeout};
use super::cursor_agent::reasoning_params::build_requested_model;
use super::cursor_agent::stream_retry::{
    cursor_stream_retry_delay_ms, wait_for_cursor_stream_retry, CursorStreamRetryCause,
};
use super::cursor_agent::types::{
    CursorAgentOptions, CursorExecHandlerResult, CursorExecHandlers, CursorExecPairing, CursorMcpCall,
    CursorPiCall, CursorShellStreamCallbacks, CursorToolResultHandler,
};
use super::cursor_agent::value_pb::{value_from_bytes, value_to_bytes};
use super::cursor_conversation_rotation::{
    is_zero_token_resource_exhausted, ConversationRotationStore, PoisonDecision, CURSOR_CONVERSATION_POISONED_MESSAGE,
};
use super::cursor_task_args::keep_usable_cursor_task_args;

pub use super::cursor_agent::types::CursorPiCall as CursorPiCallReexport;

pub const CURSOR_API_URL: &str = "https://api2.cursor.sh";
pub const CURSOR_CLIENT_VERSION: &str = "cli-2026.07.23-e383d2b";
const EXEC_HEARTBEAT_INTERVAL_MS: u64 = 3000;
/// Maximum inbound silence before a Cursor turn without turnEnded is failed.
pub const CURSOR_STREAM_HEALTH_FAIL_THRESHOLD_MS: u64 = 30_000;
/// Deprecated: heartbeats and checkpoints count as inbound liveness without a separate deadline.
pub const CURSOR_STREAM_HEALTH_HEARTBEAT_ONLY_THRESHOLD_MS: u64 = CURSOR_STREAM_HEALTH_FAIL_THRESHOLD_MS * 3;
/// Maximum time allowed to drain exec handlers after turnEnded.
pub const CURSOR_TURN_END_DRAIN_TIMEOUT_MS: u64 = 5000;

const HTTP2_FORBIDDEN_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
    "http2-settings",
];

const CURSOR_RESERVED_HEADERS: &[&str] = &[
    "content-type",
    "connect-protocol-version",
    "te",
    "authorization",
    "x-ghost-mode",
    "x-cursor-client-version",
    "x-cursor-client-type",
    "x-request-id",
    "host",
    "content-length",
];

const NOT_IMPLEMENTED_SUFFIX: &str = "not implemented by this client";
const NOT_IMPLEMENTED: &str = "Not implemented by this client";

const CONNECT_END_STREAM_FLAG: u8 = 0b0000_0010;

/// Reduce caller-supplied headers to what this HTTP/2 request can legally carry.
pub fn sanitize_cursor_caller_headers(headers: Option<&BTreeMap<String, String>>) -> BTreeMap<String, String> {
    let mut sanitized = BTreeMap::new();
    for (name, value) in headers.into_iter().flatten() {
        let field = name.to_lowercase();
        if field.starts_with(':') {
            continue;
        }
        if HTTP2_FORBIDDEN_HEADERS.contains(&field.as_str()) {
            continue;
        }
        if CURSOR_RESERVED_HEADERS.contains(&field.as_str()) {
            continue;
        }
        sanitized.insert(field, value.clone());
    }
    sanitized
}

pub fn frame_connect_message(data: &[u8], flags: u8) -> Vec<u8> {
    let mut frame = Vec::with_capacity(5 + data.len());
    frame.push(flags);
    frame.extend_from_slice(&(data.len() as u32).to_be_bytes());
    frame.extend_from_slice(data);
    frame
}

fn parse_connect_end_stream(data: &[u8]) -> Option<String> {
    let Ok(payload) = serde_json::from_slice::<Value>(data) else {
        return Some("Failed to parse Connect end stream".to_owned());
    };
    let error = payload.get("error")?;
    if error.is_null() {
        return None;
    }
    let code = error.get("code").and_then(Value::as_str).unwrap_or("unknown");
    let message = error.get("message").and_then(Value::as_str).unwrap_or("Unknown error");
    Some(format!("Connect error {code}: {message}"))
}

/// Maps an opaque HTTP/2 negotiation failure into an actionable error.
pub fn map_h2_transport_error(code: Option<&str>, message: &str, base_url: &str) -> String {
    if code == Some("ERR_HTTP2_ERROR") && message.to_lowercase().contains("h2 is not supported") {
        return format!(
            "Cursor run transport could not negotiate HTTP/2 with {base_url}: \"h2 is not supported\". \
             This host serves the run RPC over HTTP/2 only, and the TLS handshake did not negotiate \
             h2 via ALPN -- typically an ALPN-stripping TLS-intercepting proxy. \
             Front the provider with a local HTTP/2 bridge and point the model's baseUrl at it."
        );
    }
    message.to_owned()
}

fn log(kind: &str, subtype: Option<&str>, data: Option<Value>) {
    let Ok(debug) = std::env::var("DEBUG_CURSOR") else { return };
    if debug.is_empty() {
        return;
    }
    let verbose = debug == "2" || debug == "verbose";
    let mut data_str = String::new();
    if verbose && let Some(data) = data {
        let rendered = serde_json::to_string(&data).unwrap_or_else(|_| "[unserializable]".to_owned());
        data_str = format!(" {}", &rendered[..rendered.len().min(500)]);
    }
    eprintln!("[CURSOR] {kind}{}{data_str}", subtype.map(|s| format!(": {s}")).unwrap_or_default());
}

// ---------------------------------------------------------------------------
// Conversation caches
// ---------------------------------------------------------------------------

const DEFAULT_CONVERSATION_CACHE_LIMIT: usize = 64;
const DEFAULT_CONVERSATION_BLOB_LIMIT_BYTES: usize = 256 * 1024 * 1024;
const DEFAULT_CONVERSATION_TOTAL_BLOB_LIMIT_BYTES: usize = 1024 * 1024 * 1024;

fn read_positive_int_env(raw: Option<String>, fallback: usize) -> usize {
    raw.and_then(|raw| raw.parse::<f64>().ok())
        .filter(|parsed| parsed.is_finite() && *parsed > 0.0)
        .map(|parsed| parsed.floor() as usize)
        .unwrap_or(fallback)
}

fn conversation_cache_limit() -> usize {
    read_positive_int_env(std::env::var("PI_CURSOR_CONVERSATION_CACHE_LIMIT").ok(), DEFAULT_CONVERSATION_CACHE_LIMIT)
}

fn conversation_blob_limit_bytes() -> usize {
    read_positive_int_env(
        std::env::var("PI_CURSOR_CONVERSATION_BLOB_LIMIT_BYTES").ok(),
        DEFAULT_CONVERSATION_BLOB_LIMIT_BYTES,
    )
}

fn conversation_total_blob_limit_bytes() -> usize {
    read_positive_int_env(
        std::env::var("PI_CURSOR_CONVERSATION_TOTAL_BLOB_LIMIT_BYTES").ok(),
        DEFAULT_CONVERSATION_TOTAL_BLOB_LIMIT_BYTES,
    )
}

/// A conversation's blob store with byte accounting, true LRU ordering and
/// pinning for the request currently being built or streamed.
#[derive(Default)]
pub struct ConversationBlobStore {
    entries: indexmap::IndexMap<String, Vec<u8>>,
    blob_bytes: usize,
    pinned_keys: HashSet<String>,
    pin_holders: u32,
    warned_over_budget: bool,
}

impl ConversationBlobStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, key: String, value: Vec<u8>) {
        if let Some(previous) = self.entries.shift_remove(&key) {
            self.blob_bytes -= previous.len();
        }
        self.blob_bytes += value.len();
        self.entries.insert(key.clone(), value);
        if self.pin_holders > 0 {
            self.pinned_keys.insert(key);
        }
        self.trim_to_budget();
    }

    pub fn get(&mut self, key: &str) -> Option<Vec<u8>> {
        let value = self.entries.shift_remove(key)?;
        self.entries.insert(key.to_owned(), value.clone());
        if self.pin_holders > 0 {
            self.pinned_keys.insert(key.to_owned());
        }
        Some(value)
    }

    pub fn remove(&mut self, key: &str) -> bool {
        self.pinned_keys.remove(key);
        match self.entries.shift_remove(key) {
            Some(existing) => {
                self.blob_bytes -= existing.len();
                true
            }
            None => false,
        }
    }

    pub fn clear(&mut self) {
        self.blob_bytes = 0;
        self.pinned_keys.clear();
        self.entries.clear();
    }

    pub fn total_bytes(&self) -> usize {
        self.blob_bytes
    }

    pub fn pinned_count(&self) -> usize {
        self.pinned_keys.len()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn keys(&self) -> Vec<String> {
        self.entries.keys().cloned().collect()
    }

    pub fn begin_request_pins(&mut self) {
        self.pin_holders += 1;
    }

    /// Evicts unpinned blobs, least recently used first, until at most `keep_bytes` remain.
    pub fn shed_unpinned_bytes(&mut self, keep_bytes: usize) -> usize {
        let mut released = 0;
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for key in keys {
            if self.blob_bytes <= keep_bytes {
                break;
            }
            if self.pinned_keys.contains(&key) {
                continue;
            }
            if let Some(value) = self.entries.shift_remove(&key) {
                self.blob_bytes -= value.len();
                released += value.len();
            }
        }
        released
    }

    pub fn release_request_pins(&mut self) {
        if self.pin_holders == 0 {
            return;
        }
        self.pin_holders -= 1;
        if self.pin_holders > 0 {
            return;
        }
        self.pinned_keys.clear();
        self.warned_over_budget = false;
        self.trim_to_budget();
    }

    fn trim_to_budget(&mut self) {
        let limit = conversation_blob_limit_bytes();
        if self.blob_bytes <= limit {
            return;
        }
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for key in keys {
            if self.blob_bytes <= limit {
                break;
            }
            if self.pinned_keys.contains(&key) {
                continue;
            }
            if let Some(value) = self.entries.shift_remove(&key) {
                self.blob_bytes -= value.len();
            }
        }
        if self.blob_bytes <= limit || self.warned_over_budget {
            return;
        }
        self.warned_over_budget = true;
        eprintln!(
            "[cursor-agent] conversation blob store over budget: {} bytes pinned by the in-flight request exceed the {} byte cap ({} pinned blobs); the cap applies again when the stream settles",
            self.blob_bytes,
            limit,
            self.pinned_keys.len()
        );
    }
}

struct ConversationCaches {
    state: HashMap<String, ConversationStateStructure>,
    blobs: HashMap<String, Arc<Mutex<ConversationBlobStore>>>,
    key_sessions: HashMap<String, String>,
    live_keys: HashMap<String, u32>,
}

static CONVERSATIONS: LazyLock<Mutex<ConversationCaches>> = LazyLock::new(|| {
    let caches = ConversationCaches {
        state: HashMap::new(),
        blobs: HashMap::new(),
        key_sessions: HashMap::new(),
        live_keys: HashMap::new(),
    };
    Mutex::new(caches)
});

fn conversations() -> MutexGuard<'static, ConversationCaches> {
    CONVERSATIONS.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn register_conversation_cache_key(session_id: Option<&str>, conversation_id: &str) {
    let Some(session_id) = session_id else { return };
    conversations().key_sessions.insert(conversation_id.to_owned(), session_id.to_owned());
}

fn forget_conversation_cache_key(conversation_id: &str) {
    let mut caches = conversations();
    caches.state.remove(conversation_id);
    caches.blobs.remove(conversation_id);
    caches.key_sessions.remove(conversation_id);
}

fn retain_live_conversation(conversation_id: &str) {
    let mut caches = conversations();
    *caches.live_keys.entry(conversation_id.to_owned()).or_insert(0) += 1;
}

fn release_live_conversation(conversation_id: &str) {
    let mut caches = conversations();
    match caches.live_keys.get(conversation_id).copied() {
        Some(holders) if holders > 1 => {
            caches.live_keys.insert(conversation_id.to_owned(), holders - 1);
        }
        Some(_) => {
            caches.live_keys.remove(conversation_id);
        }
        None => {}
    }
}

/// Trims the overflowing session's own conversations, oldest first.
fn enforce_conversation_cache_limit(newest_key: &str, session_id: Option<&str>) {
    let limit = conversation_cache_limit();
    let mut caches = conversations();
    let owner = session_id
        .map(str::to_owned)
        .or_else(|| caches.key_sessions.get(newest_key).cloned());
    let owned_keys: Vec<String> = caches
        .blobs
        .keys()
        .filter(|key| caches.key_sessions.get(*key).cloned() == owner)
        .cloned()
        .collect();
    let mut owned = owned_keys.len();
    for key in owned_keys {
        if owned <= limit {
            return;
        }
        if key == newest_key {
            continue;
        }
        if caches.live_keys.contains_key(&key) {
            continue;
        }
        caches.state.remove(&key);
        caches.blobs.remove(&key);
        caches.key_sessions.remove(&key);
        owned -= 1;
    }
}

fn enforce_conversation_total_blob_limit() {
    let limit = conversation_total_blob_limit_bytes();
    let (cold, live, mut total) = {
        let caches = conversations();
        let mut total = 0usize;
        let mut cold = Vec::new();
        let mut live = Vec::new();
        for (key, store) in &caches.blobs {
            let bytes = store.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).total_bytes();
            total += bytes;
            if caches.live_keys.contains_key(key) {
                live.push((key.clone(), store.clone()));
            } else {
                cold.push((key.clone(), store.clone()));
            }
        }
        (cold, live, total)
    };
    if total <= limit {
        return;
    }
    for (key, store) in cold.into_iter().chain(live) {
        if total <= limit {
            break;
        }
        let (released, empty) = {
            let mut store = store.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let keep = store.total_bytes().saturating_sub(total - limit);
            (store.shed_unpinned_bytes(keep), store.is_empty())
        };
        total -= released;
        if empty && !conversations().live_keys.contains_key(&key) {
            forget_conversation_cache_key(&key);
        }
    }
    if total <= limit {
        return;
    }
    eprintln!(
        "[cursor-agent] cursor conversation blobs total {total} bytes over the {limit} byte process ceiling; the remainder is pinned by in-flight requests and is released when their streams settle"
    );
}

fn get_or_create_conversation_blob_store(
    conversation_id: &str,
    session_id: Option<&str>,
) -> Arc<Mutex<ConversationBlobStore>> {
    let existing = conversations().blobs.get(conversation_id).cloned();
    if let Some(existing) = existing {
        return existing;
    }
    let store = Arc::new(Mutex::new(ConversationBlobStore::new()));
    conversations().blobs.insert(conversation_id.to_owned(), store.clone());
    enforce_conversation_cache_limit(conversation_id, session_id);
    store
}

fn release_conversation_cache_for_session(session_id: Option<&str>) {
    let mut caches = conversations();
    let Some(session_id) = session_id else {
        caches.state.clear();
        caches.blobs.clear();
        caches.key_sessions.clear();
        caches.live_keys.clear();
        return;
    };
    let keys: Vec<String> = caches
        .key_sessions
        .iter()
        .filter(|(_, owner)| owner.as_str() == session_id)
        .map(|(key, _)| key.clone())
        .collect();
    for key in keys {
        caches.state.remove(&key);
        caches.blobs.remove(&key);
        caches.key_sessions.remove(&key);
    }
}

static SESSION_CLEANUP_REGISTERED: LazyLock<()> = LazyLock::new(|| {
    let _ = register_session_resource_cleanup(Arc::new(|session_id: Option<&str>| {
        release_conversation_cache_for_session(session_id);
        Ok(())
    }));
});

/// Size telemetry for the conversation caches (#1024 verification seam).
pub struct CursorConversationCacheStats {
    pub conversations: usize,
    pub states: usize,
    pub blob_bytes: usize,
    pub keys: Vec<String>,
}

pub fn get_cursor_conversation_cache_stats() -> CursorConversationCacheStats {
    LazyLock::force(&SESSION_CLEANUP_REGISTERED);
    let caches = conversations();
    let mut blob_bytes = 0;
    for store in caches.blobs.values() {
        blob_bytes += store.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).total_bytes();
    }
    CursorConversationCacheStats {
        conversations: caches.blobs.len(),
        states: caches.state.len(),
        blob_bytes,
        keys: caches.blobs.keys().cloned().collect(),
    }
}

/// Base conversation id -> rotated wire id. Cursor's backend can pin a
/// per-conversation rejection (bare `resource_exhausted`, zero tokens) to one
/// conversationId forever. On the first such failure the id is rotated once and
/// the cached state migrates, so the retry loop's next attempt starts a fresh
/// conversation.
struct RotationStoreHolder {
    store: ConversationRotationStore<fn() -> String>,
    persist_path: String,
}

static ROTATION: LazyLock<Mutex<RotationStoreHolder>> = LazyLock::new(|| {
    let persist_path = super::cursor_conversation_rotation::resolve_conversation_rotation_persist_path(
        &std::env::vars().collect(),
    );
    Mutex::new(RotationStoreHolder {
        store: ConversationRotationStore::new(persist_path.clone().into()),
        persist_path,
    })
});

fn rotation_store() -> MutexGuard<'static, RotationStoreHolder> {
    let mut holder = ROTATION.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let persist_path = super::cursor_conversation_rotation::resolve_conversation_rotation_persist_path(
        &std::env::vars().collect(),
    );
    if persist_path != holder.persist_path {
        holder.persist_path = persist_path.clone();
        holder.store = ConversationRotationStore::new(persist_path.into());
    }
    holder
}

// ---------------------------------------------------------------------------
// Blob ids
// ---------------------------------------------------------------------------

fn create_blob_id(data: &[u8]) -> Vec<u8> {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hasher.finalize().to_vec()
}

fn store_cursor_blob(blob_store: &Arc<Mutex<ConversationBlobStore>>, data: Vec<u8>) -> Vec<u8> {
    let blob_id = create_blob_id(&data);
    blob_store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .set(hex::encode(&blob_id), data);
    blob_id
}

// ---------------------------------------------------------------------------
// Tool-result helpers
// ---------------------------------------------------------------------------

fn tool_result_to_text(tool_result: &ToolResultMessage) -> String {
    tool_result
        .content
        .iter()
        .map(|item| match item {
            ContentBlock::Text(text) => text.text.clone(),
            ContentBlock::Image(image) => format!("[{} image]", image.mime_type),
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                "[undefined image]".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_result_was_truncated(tool_result: &ToolResultMessage) -> bool {
    tool_result
        .details
        .as_ref()
        .and_then(|details| details.get("truncation"))
        .and_then(|truncation| truncation.get("truncated"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn tool_result_detail_boolean(tool_result: &ToolResultMessage, key: &str) -> bool {
    tool_result
        .details
        .as_ref()
        .and_then(|details| details.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn read_total_lines_from_details(tool_result: &ToolResultMessage) -> Option<i64> {
    let details = tool_result.details.as_ref()?;
    if let Some(direct) = details.get("totalLines").and_then(Value::as_f64)
        && direct.is_finite()
    {
        return Some(direct as i64);
    }
    let meta = details.get("meta")?;
    let truncation = meta.get("truncation")?;
    truncation.get("totalLines").and_then(Value::as_f64).filter(|value| value.is_finite()).map(|value| value as i64)
}

fn read_file_size_from_details(tool_result: &ToolResultMessage) -> Option<f64> {
    let details = tool_result.details.as_ref()?;
    let file_size = details.get("fileSize")?.as_f64()?;
    if file_size.is_finite() && file_size >= 0.0 && file_size.fract() == 0.0 {
        Some(file_size)
    } else {
        None
    }
}

fn build_read_result_from_tool_result(path: &str, tool_result: &ToolResultMessage, range_applied: bool) -> ReadResult {
    let text = tool_result_to_text(tool_result);
    if tool_result.is_error {
        return build_read_error_result(path, if text.is_empty() { "Read failed" } else { &text });
    }
    let total_lines = read_total_lines_from_details(tool_result).unwrap_or(if range_applied || text.is_empty() {
        0
    } else {
        text.split('\n').count() as i64
    });
    let file_size = read_file_size_from_details(tool_result).unwrap_or(text.len() as f64);
    ReadResult {
        result: Some(read_result::Result::Success(ReadSuccess {
            path: path.to_owned(),
            total_lines: total_lines as i32,
            file_size: file_size as i64,
            truncated: tool_result_was_truncated(tool_result),
            output: Some(read_success::Output::Content(text)),
            range_applied,
            ..ReadSuccess::default()
        })),
    }
}

fn build_read_error_result(path: &str, error: &str) -> ReadResult {
    ReadResult {
        result: Some(read_result::Result::Error(ReadError { path: path.to_owned(), error: error.to_owned() })),
    }
}

fn build_read_rejected_result(path: &str, reason: &str) -> ReadResult {
    ReadResult {
        result: Some(read_result::Result::Rejected(ReadRejected { path: path.to_owned(), reason: reason.to_owned() })),
    }
}

struct WriteResultArgs<'a> {
    path: &'a str,
    file_text: Option<&'a str>,
    file_bytes: Option<&'a [u8]>,
    return_file_content_after_write: Option<bool>,
}

fn build_write_result_from_tool_result(args: &WriteResultArgs<'_>, tool_result: &ToolResultMessage) -> WriteResult {
    let text = tool_result_to_text(tool_result);
    if tool_result.is_error {
        return build_write_error_result(args.path, if text.is_empty() { "Write failed" } else { &text });
    }
    let file_text = args.file_text.unwrap_or("");
    let file_size = args.file_bytes.map(<[u8]>::len).unwrap_or_else(|| file_text.len());
    let lines_created = if file_text.is_empty() { 0 } else { file_text.split('\n').count() };
    WriteResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::write_result::Result::Success(WriteSuccess {
            path: args.path.to_owned(),
            lines_created: lines_created as i32,
            file_size: file_size as i32,
            file_content_after_write: args.return_file_content_after_write.unwrap_or(false).then(|| file_text.to_owned()),
        })),
    }
}

fn build_write_error_result(path: &str, error: &str) -> WriteResult {
    WriteResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::write_result::Result::Error(WriteError {
            path: path.to_owned(),
            error: error.to_owned(),
        })),
    }
}

fn build_write_rejected_result(path: &str, reason: &str) -> WriteResult {
    WriteResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::write_result::Result::Rejected(WriteRejected {
            path: path.to_owned(),
            reason: reason.to_owned(),
        })),
    }
}

fn build_delete_result_from_tool_result(path: &str, tool_result: &ToolResultMessage) -> DeleteResult {
    let text = tool_result_to_text(tool_result);
    if tool_result.is_error {
        return build_delete_error_result(path, if text.is_empty() { "Delete failed" } else { &text });
    }
    DeleteResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::delete_result::Result::Success(DeleteSuccess {
            path: path.to_owned(),
            deleted_file: path.to_owned(),
            file_size: 0,
            prev_content: String::new(),
        })),
    }
}

fn build_delete_error_result(path: &str, error: &str) -> DeleteResult {
    DeleteResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::delete_result::Result::Error(DeleteError {
            path: path.to_owned(),
            error: error.to_owned(),
        })),
    }
}

fn build_delete_rejected_result(path: &str, reason: &str) -> DeleteResult {
    DeleteResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::delete_result::Result::Rejected(DeleteRejected {
            path: path.to_owned(),
            reason: reason.to_owned(),
        })),
    }
}

fn build_shell_result_from_tool_result(args: &ShellArgs, tool_result: &ToolResultMessage) -> ShellResult {
    let output = tool_result_to_text(tool_result);
    if tool_result.is_error {
        return build_shell_failure_result(
            &args.command,
            &args.working_directory,
            if output.is_empty() { "Shell failed" } else { &output },
        );
    }
    ShellResult {
        result: Some(shell_result::Result::Success(ShellSuccess {
            command: args.command.clone(),
            working_directory: args.working_directory.clone(),
            exit_code: 0,
            signal: String::new(),
            stdout: output,
            stderr: String::new(),
            execution_time: 0,
            ..ShellSuccess::default()
        })),
        ..ShellResult::default()
    }
}

fn build_shell_failure_result(command: &str, working_directory: &str, error: &str) -> ShellResult {
    ShellResult {
        result: Some(shell_result::Result::Failure(ShellFailure {
            command: command.to_owned(),
            working_directory: working_directory.to_owned(),
            exit_code: 1,
            signal: String::new(),
            stdout: String::new(),
            stderr: error.to_owned(),
            execution_time: 0,
            aborted: false,
            ..ShellFailure::default()
        })),
        ..ShellResult::default()
    }
}

fn build_shell_rejected_result(command: &str, working_directory: &str, reason: &str) -> ShellResult {
    ShellResult {
        result: Some(shell_result::Result::Rejected(ShellRejected {
            command: command.to_owned(),
            working_directory: working_directory.to_owned(),
            reason: reason.to_owned(),
            is_readonly: false,
        })),
        ..ShellResult::default()
    }
}

fn build_ls_result_from_tool_result(path: &str, tool_result: &ToolResultMessage) -> LsResult {
    let text = tool_result_to_text(tool_result);
    if tool_result.is_error {
        return build_ls_error_result(path, if text.is_empty() { "Ls failed" } else { &text });
    }
    let root_path = if path.is_empty() { "." } else { path };
    let entries: Vec<&str> = text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('['))
        .collect();
    let mut children_dirs = Vec::new();
    let mut children_files = Vec::new();
    for entry in entries {
        let name = entry.split(" (").next().unwrap_or(entry);
        if let Some(dir_name) = name.strip_suffix('/') {
            children_dirs.push(LsDirectoryTreeNode {
                abs_path: format!("{}/{}", root_path.trim_end_matches('/'), dir_name),
                children_dirs: Vec::new(),
                children_files: Vec::new(),
                children_were_processed: false,
                full_subtree_extension_counts: HashMap::new(),
                num_files: 0,
            });
        } else {
            children_files.push(LsDirectoryTreeNodeFile { name: name.to_owned(), terminal_metadata: None });
        }
    }
    let num_files = children_files.len() as i32;
    let root = LsDirectoryTreeNode {
        abs_path: root_path.to_owned(),
        children_dirs,
        children_files,
        children_were_processed: true,
        full_subtree_extension_counts: HashMap::new(),
        num_files,
    };
    LsResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::ls_result::Result::Success(LsSuccess {
            directory_tree_root: Some(root),
        })),
    }
}

fn build_ls_error_result(path: &str, error: &str) -> LsResult {
    LsResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::ls_result::Result::Error(LsError {
            path: path.to_owned(),
            error: error.to_owned(),
        })),
    }
}

fn build_ls_rejected_result(path: &str, reason: &str) -> LsResult {
    LsResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::ls_result::Result::Rejected(LsRejected {
            path: path.to_owned(),
            reason: reason.to_owned(),
        })),
    }
}

fn build_grep_result_from_tool_result(args: &GrepArgs, tool_result: &ToolResultMessage) -> GrepResult {
    let text = tool_result_to_text(tool_result);
    if tool_result.is_error {
        return build_grep_error_result(if text.is_empty() { "Grep failed" } else { &text });
    }
    let output_mode = args.output_mode.as_deref().filter(|mode| !mode.is_empty()).unwrap_or("content").to_owned();
    let client_truncated = tool_result_detail_boolean(tool_result, "truncated");
    let args_offset = args.offset;
    let lines: Vec<&str> = text
        .split('\n')
        .map(str::trim_end)
        .filter(|line| {
            !line.is_empty() && !line.starts_with('[') && !line.to_lowercase().starts_with("no matches")
        })
        .collect();
    let workspace_key = args.path.as_deref().filter(|path| !path.is_empty()).unwrap_or(".").to_owned();

    let union_result = if output_mode == "files_with_matches" {
        let files: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
        GrepUnionResult {
            result: Some(super::cursor_agent::r#gen::agent_pb::grep_union_result::Result::Files(GrepFilesResult {
                total_files: files.len() as i32,
                files,
                client_truncated,
                ripgrep_truncated: false,
                offset_applied: args_offset,
                ..GrepFilesResult::default()
            })),
        }
    } else if output_mode == "count" {
        let counts: Vec<GrepFileCount> = lines
            .iter()
            .filter_map(|line| {
                let separator_index = line.rfind(':')?;
                let file = &line[..separator_index];
                let count = line[separator_index + 1..].trim().parse::<i32>().ok()?;
                if file.is_empty() {
                    return None;
                }
                Some(GrepFileCount { file: file.to_owned(), count })
            })
            .collect();
        let total_matches: i32 = counts.iter().map(|entry| entry.count).sum();
        GrepUnionResult {
            result: Some(super::cursor_agent::r#gen::agent_pb::grep_union_result::Result::Count(GrepCountResult {
                total_files: counts.len() as i32,
                total_matches,
                counts,
                client_truncated,
                ripgrep_truncated: false,
                offset_applied: args_offset,
                head_limit_applied: None,
            })),
        }
    } else {
        let mut match_map: indexmap::IndexMap<String, Vec<(i32, String, bool)>> = indexmap::IndexMap::new();
        let mut total_matched_lines = 0;
        for line in &lines {
            let Some((file, line_number, content, is_context_line)) = parse_grep_content_line(line) else {
                continue;
            };
            if !is_context_line {
                total_matched_lines += 1;
            }
            match_map.entry(file).or_default().push((line_number, content, is_context_line));
        }
        let matches: Vec<GrepFileMatch> = match_map
            .into_iter()
            .map(|(file, file_matches)| GrepFileMatch {
                file,
                matches: file_matches
                    .into_iter()
                    .map(|(line_number, content, is_context_line)| GrepContentMatch {
                        line_number,
                        content,
                        content_truncated: false,
                        is_context_line,
                    })
                    .collect(),
            })
            .collect();
        let total_lines: i32 = matches.iter().map(|entry| entry.matches.len() as i32).sum();
        GrepUnionResult {
            result: Some(super::cursor_agent::r#gen::agent_pb::grep_union_result::Result::Content(GrepContentResult {
                total_lines,
                total_matched_lines,
                matches,
                client_truncated,
                ripgrep_truncated: false,
                offset_applied: args_offset,
                ..GrepContentResult::default()
            })),
        }
    };

    GrepResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::grep_result::Result::Success(GrepSuccess {
            pattern: args.pattern.clone(),
            path: args.path.clone().unwrap_or_default(),
            output_mode: output_mode.to_owned(),
            workspace_results: HashMap::from([(workspace_key, union_result)]),
            ..GrepSuccess::default()
        })),
    }
}

/// `line.match(/^(.+?):(\d+):\s?(.*)$/)` then `line.match(/^(.+?)-(\d+)-\s?(.*)$/)`.
fn parse_grep_content_line(line: &str) -> Option<(String, i32, String, bool)> {
    if let Some((file, line_number, content)) = match_grep_line(line, ':') {
        return Some((file, line_number, content, false));
    }
    match_grep_line(line, '-').map(|(file, line_number, content)| (file, line_number, content, true))
}

fn match_grep_line(line: &str, separator: char) -> Option<(String, i32, String)> {
    let bytes: Vec<char> = line.chars().collect();
    for (index, character) in bytes.iter().enumerate() {
        if index == 0 || *character != separator {
            continue;
        }
        let rest: String = bytes[index + 1..].iter().collect();
        let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            continue;
        }
        let after = &rest[digits.len()..];
        let content = after.strip_prefix(' ').unwrap_or(after);
        if let Ok(line_number) = digits.parse::<i32>() {
            return Some((bytes[..index].iter().collect(), line_number, content.to_owned()));
        }
    }
    None
}

fn build_grep_error_result(error: &str) -> GrepResult {
    GrepResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::grep_result::Result::Error(GrepError {
            error: error.to_owned(),
        })),
    }
}

/// Reject a Cursor exec-channel `grepArgs` frame whose `pattern` is empty or
/// whitespace-only. Returns an actionable error message (with a `glob`-aware
/// hint when the model likely meant to list files) or `None` when the pattern
/// is valid and grep should run. Exported for tests.
pub fn empty_grep_pattern_rejection(pattern: Option<&str>, glob: Option<&str>) -> Option<String> {
    if pattern.is_some_and(|pattern| !pattern.trim().is_empty()) {
        return None;
    }
    if glob.is_some_and(|glob| !glob.is_empty()) {
        let glob = glob.unwrap_or_default();
        return Some(format!(
            "grep pattern is required (received an empty pattern). To list files matching \"{glob}\", \
             pass a non-empty regex (e.g. \".\") and set path to that glob, or use the ls/read tool instead."
        ));
    }
    Some("grep pattern is required (received an empty pattern).".to_owned())
}

fn build_diagnostics_result_from_tool_result(path: &str, tool_result: &ToolResultMessage) -> DiagnosticsResult {
    let text = tool_result_to_text(tool_result);
    if tool_result.is_error {
        return build_diagnostics_error_result(if text.is_empty() { "Diagnostics failed" } else { &text });
    }
    DiagnosticsResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::diagnostics_result::Result::Success(DiagnosticsSuccess {
            path: path.to_owned(),
            diagnostics: Vec::new(),
            total_diagnostics: 0,
        })),
    }
}

fn build_diagnostics_error_result(error: &str) -> DiagnosticsResult {
    DiagnosticsResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::diagnostics_result::Result::Error(DiagnosticsError {
            path: String::new(),
            error: error.to_owned(),
        })),
    }
}

fn build_diagnostics_rejected_result(path: &str, reason: &str) -> DiagnosticsResult {
    DiagnosticsResult {
        result: Some(super::cursor_agent::r#gen::agent_pb::diagnostics_result::Result::Rejected(DiagnosticsRejected {
            path: path.to_owned(),
            reason: reason.to_owned(),
        })),
    }
}

fn parse_tool_args_json(text: &str) -> Value {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Value::String(text.to_owned());
    }
    parse_json_with_repair::<Value>(trimmed).unwrap_or_else(|_| Value::String(text.to_owned()))
}

fn decode_mcp_arg_value(value: &[u8]) -> Value {
    if let Ok(parsed) = value_from_bytes(value) {
        if let Value::String(text) = &parsed {
            return parse_tool_args_json(text);
        }
        return parsed;
    }
    parse_tool_args_json(&String::from_utf8_lossy(value))
}

fn decode_mcp_args_map(args: Option<&HashMap<String, Bytes>>) -> Option<Map<String, Value>> {
    let args = args?;
    Some(args.iter().map(|(key, value)| (key.clone(), decode_mcp_arg_value(value))).collect())
}

fn decode_mcp_call(args: &McpArgs) -> CursorMcpCall {
    let decoded_args: Map<String, Value> =
        args.args.iter().map(|(key, value)| (key.clone(), decode_mcp_arg_value(value))).collect();
    CursorMcpCall {
        name: args.name.clone(),
        provider_identifier: args.provider_identifier.clone(),
        tool_name: if args.tool_name.is_empty() { args.name.clone() } else { args.tool_name.clone() },
        tool_call_id: args.tool_call_id.clone(),
        args: decoded_args,
        raw_args: args.args.iter().map(|(key, value)| (key.clone(), value.to_vec())).collect(),
        approval_only: Some(args.smart_mode_approval_only),
    }
}

/// Map Cursor's `TodoStatus` enum onto display statuses.
fn map_todo_status_value(status: Option<i32>) -> &'static str {
    match status {
        Some(2) => "in_progress",
        Some(3) => "completed",
        Some(4) => "abandoned",
        _ => "pending",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TodoSnapshot {
    todos: Vec<TodoDisplayItem>,
    merged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TodoDisplayItem {
    content: String,
    status: String,
}

fn map_todo_snapshot(todos: &[super::cursor_agent::r#gen::agent_pb::TodoItem]) -> Vec<TodoDisplayItem> {
    todos
        .iter()
        .map(|todo| TodoDisplayItem {
            content: todo.content.clone(),
            status: map_todo_status_value(Some(todo.status)).to_owned(),
        })
        .collect()
}

fn select_todo_calls(tool_call: &PbToolCall) -> (Option<&UpdateTodosToolCall>, Option<&ReadTodosToolCall>) {
    match &tool_call.tool {
        Some(tool_call::Tool::UpdateTodosToolCall(call)) => (Some(call), None),
        Some(tool_call::Tool::ReadTodosToolCall(call)) => (None, Some(call)),
        _ => (None, None),
    }
}

fn select_mcp_call(tool_call: Option<&PbToolCall>) -> Option<&McpToolCall> {
    match tool_call?.tool.as_ref()? {
        tool_call::Tool::McpToolCall(call) => Some(call),
        _ => None,
    }
}

/// The streamed `ToolCall` variants whose block the exec channel owns.
fn is_exec_owned_tool_call(tool_call: Option<&PbToolCall>) -> bool {
    matches!(
        tool_call.and_then(|call| call.tool.as_ref()),
        Some(
            tool_call::Tool::PiReadToolCall(_)
                | tool_call::Tool::PiBashToolCall(_)
                | tool_call::Tool::PiEditToolCall(_)
                | tool_call::Tool::PiWriteToolCall(_)
                | tool_call::Tool::PiGrepToolCall(_)
                | tool_call::Tool::PiFindToolCall(_)
                | tool_call::Tool::PiLsToolCall(_)
                | tool_call::Tool::ListMcpResourcesToolCall(_)
                | tool_call::Tool::ReadMcpResourceToolCall(_)
        )
    )
}

fn select_connect_scm_call(tool_call: Option<&PbToolCall>) -> Option<&ConnectScmToolCall> {
    match tool_call?.tool.as_ref()? {
        tool_call::Tool::ConnectScmToolCall(call) => Some(call),
        _ => None,
    }
}

fn select_connect_scm_repository(call: Option<&ConnectScmToolCall>) -> Option<&ConnectScmGithubRepository> {
    match call?.args.as_ref()?.target.as_ref()? {
        super::cursor_agent::r#gen::agent_pb::connect_scm_args::Target::Github(github) => {
            github.repository.as_ref()
        }
    }
}

/// Render a settled `ConnectScmResult` as the text of its paired tool result.
fn describe_connect_scm_result(call: Option<&ConnectScmToolCall>) -> (String, bool) {
    match call.and_then(|call| call.result.as_ref()).and_then(|result| result.result.as_ref()) {
        Some(super::cursor_agent::r#gen::agent_pb::connect_scm_result::Result::Success(_)) => {
            ("SCM connected".to_owned(), false)
        }
        Some(super::cursor_agent::r#gen::agent_pb::connect_scm_result::Result::Error(error)) => {
            (if error.error.is_empty() { "SCM connection failed".to_owned() } else { error.error.clone() }, true)
        }
        Some(super::cursor_agent::r#gen::agent_pb::connect_scm_result::Result::Rejected(rejected)) => (
            if rejected.reason.is_empty() { "SCM connection rejected".to_owned() } else { rejected.reason.clone() },
            true,
        ),
        None => ("SCM connection reported no result".to_owned(), true),
    }
}

fn extract_todo_snapshot(tool_call: &PbToolCall) -> Option<TodoSnapshot> {
    let (update, read) = select_todo_calls(tool_call);
    if read.is_some_and(|read| {
        read.args.as_ref().is_some_and(|args| {
            !args.status_filter.is_empty() || !args.id_filter.is_empty()
        })
    }) {
        return None;
    }
    let (todos, merged) = match (update, read) {
        (Some(update), _) => {
            let super::cursor_agent::r#gen::agent_pb::update_todos_result::Result::Success(success) =
                update.result.as_ref()?.result.as_ref()?
            else {
                return None;
            };
            if success.total_count != 0 && success.total_count as usize != success.todos.len() {
                return None;
            }
            (&success.todos, success.was_merge)
        }
        (None, Some(read)) => {
            let super::cursor_agent::r#gen::agent_pb::read_todos_result::Result::Success(success) =
                read.result.as_ref()?.result.as_ref()?
            else {
                return None;
            };
            if success.total_count != 0 && success.total_count as usize != success.todos.len() {
                return None;
            }
            if success.todos.is_empty() {
                return None;
            }
            (&success.todos, false)
        }
        (None, None) => return None,
    };
    let mapped = map_todo_snapshot(todos);
    if mapped.iter().any(|todo| todo.content.is_empty()) {
        return None;
    }
    Some(TodoSnapshot { todos: mapped, merged })
}

/// Error text when the server itself rejected the call.
fn extract_todo_error(tool_call: &PbToolCall) -> Option<String> {
    let (update, read) = select_todo_calls(tool_call);
    let error = match (update, read) {
        (Some(update), _) => {
            let super::cursor_agent::r#gen::agent_pb::update_todos_result::Result::Error(error) =
                update.result.as_ref()?.result.as_ref()?
            else {
                return None;
            };
            error.error.clone()
        }
        (None, Some(read)) => {
            let super::cursor_agent::r#gen::agent_pb::read_todos_result::Result::Error(error) =
                read.result.as_ref()?.result.as_ref()?
            else {
                return None;
            };
            error.error.clone()
        }
        (None, None) => return None,
    };
    Some(if error.is_empty() { "Todo operation failed".to_owned() } else { error })
}

/// Args echoed onto the synthesized display block, for rendering only.
fn build_todo_display_args(tool_call: &PbToolCall) -> Map<String, Value> {
    let args = select_todo_calls(tool_call).0.and_then(|update| update.args.as_ref());
    let mut object = Map::new();
    object.insert(
        "todos".to_owned(),
        Value::Array(
            args.map(|args| map_todo_snapshot(&args.todos))
                .unwrap_or_default()
                .into_iter()
                .map(|todo| json!({ "content": todo.content, "status": todo.status }))
                .collect(),
        ),
    );
    if args.is_some_and(|args| args.r#merge) {
        object.insert("merge".to_owned(), Value::Bool(true));
    }
    object
}

fn build_todo_tool_result(tool_call_id: &str, snapshot: Option<&TodoSnapshot>, error: Option<&str>) -> ToolResultMessage {
    let text = error
        .map(str::to_owned)
        .unwrap_or_else(|| snapshot.map(|snapshot| format_todo_snapshot_summary(&snapshot.todos)).unwrap_or_else(|| "Todo snapshot not mirrored".to_owned()));
    ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: "todo".to_owned(),
        content: vec![ContentBlock::text(text)],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error: error.is_some(),
        timestamp: now_ms(),
    }
}

fn format_todo_snapshot_summary(todos: &[TodoDisplayItem]) -> String {
    if todos.is_empty() {
        return "No todos".to_owned();
    }
    let done = todos.iter().filter(|todo| todo.status == "completed").count();
    format!("{done}/{} tasks completed", todos.len())
}

fn build_mcp_result_from_tool_result(tool_result: &ToolResultMessage) -> McpResult {
    if tool_result.is_error {
        let text = tool_result_to_text(tool_result);
        return build_mcp_error_result(if text.is_empty() { "MCP tool failed" } else { &text });
    }
    let content = tool_result
        .content
        .iter()
        .map(|item| match item {
            ContentBlock::Image(image) => McpToolResultContentItem {
                content: Some(mcp_tool_result_content_item::Content::Image(McpImageContent {
                    data: base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap_or_default().into(),
                    mime_type: image.mime_type.clone(),
                })),
            },
            ContentBlock::Text(text) => McpToolResultContentItem {
                content: Some(mcp_tool_result_content_item::Content::Text(McpTextContent {
                    text: text.text.clone(),
                    ..McpTextContent::default()
                })),
            },
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                McpToolResultContentItem {
                    content: Some(mcp_tool_result_content_item::Content::Text(McpTextContent {
                        text: "[undefined image]".to_owned(),
                        ..McpTextContent::default()
                    })),
                }
            }
        })
        .collect();
    McpResult { result: Some(mcp_result::Result::Success(McpSuccess { content, is_error: false })) }
}

fn build_mcp_tool_not_found_result(mcp_call: &CursorMcpCall) -> McpResult {
    McpResult {
        result: Some(mcp_result::Result::ToolNotFound(McpToolNotFound {
            name: mcp_call.tool_name.clone(),
            available_tools: Vec::new(),
        })),
    }
}

fn build_mcp_error_result(error: &str) -> McpResult {
    McpResult { result: Some(mcp_result::Result::Error(McpError { error: error.to_owned() })) }
}

/// Merge the decoded completion-frame `McpArgs` map into the args assembled
/// from streamed `args_text_delta` snapshots.
pub fn merge_cursor_mcp_tool_call_args(
    streamed: Option<&Map<String, Value>>,
    completion: Option<&Map<String, Value>>,
) -> Map<String, Value> {
    let mut merged = streamed.cloned().unwrap_or_default();
    let Some(completion) = completion else { return merged };
    for (key, completion_value) in completion {
        let streamed_value = merged.get(key);
        // JS typeof on an array is also "object", so a string completion must
        // not downgrade a streamed array either.
        if completion_value.is_string() && streamed_value.is_some_and(|value| value.is_object() || value.is_array()) {
            continue;
        }
        merged.insert(key.clone(), completion_value.clone());
    }
    merged
}

fn end_current_text_block(output: &mut AssistantMessage, stream: &AssistantMessageEventStream, state: &mut BlockState) {
    let Some(index) = state.current_text_index else { return };
    let content = match output.content.get(index) {
        Some(ContentBlock::Text(block)) => block.text.clone(),
        _ => String::new(),
    };
    stream.push(AssistantMessageEvent::TextEnd { content_index: index, content, partial: output.clone() });
    state.current_text_block = None;
    state.current_text_index = None;
}

fn end_current_thinking_block(
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    state: &mut BlockState,
) {
    let Some(index) = state.current_thinking_index else { return };
    let content = match output.content.get(index) {
        Some(ContentBlock::Thinking(block)) => block.thinking.clone(),
        _ => String::new(),
    };
    stream.push(AssistantMessageEvent::ThinkingEnd { content_index: index, content, partial: output.clone() });
    state.current_thinking_block = None;
    state.current_thinking_index = None;
}

/// Ensure a Cursor exec frame's tool-call id is present and unique within the
/// assistant message before a block is synthesized from it. Exported for tests.
pub fn ensure_unique_cursor_exec_tool_call_id(output: &AssistantMessage, tool_call_id: &mut Option<String>) {
    let Some(base) = tool_call_id.clone().filter(|id| !id.is_empty()) else {
        *tool_call_id = Some(random_uuid());
        return;
    };
    let mut candidate = base.clone();
    let mut suffix = 2;
    while output.content.iter().any(|block| match block {
        ContentBlock::ToolCall(call) => call.id == candidate,
        _ => false,
    }) {
        candidate = format!("{base}-{suffix}");
        suffix += 1;
    }
    *tool_call_id = Some(candidate);
}

/// Synthesize a completed `toolCall` content block for a Cursor exec-channel
/// native tool or for an MCP exec frame whose corresponding interaction block is
/// absent. Exported for tests.
pub fn synthesize_cursor_exec_tool_call(
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    state: &mut BlockState,
    tool_call_id: String,
    tool_name: &str,
    args: Map<String, Value>,
) {
    end_current_text_block(output, stream, state);
    end_current_thinking_block(output, stream, state);
    let block = ContentBlock::ToolCall(ToolCall {
        id: tool_call_id,
        name: tool_name.to_owned(),
        arguments: omit_undefined_args(args),
        incomplete: None,
        error_message: None,
        thought_signature: None,
        namespace: None,
    });
    let index = output.content.len();
    output.content.push(block.clone());
    state.streaming.insert(
        index,
        StreamingBlockState { block_index: Some(index), block_kind: Some("cursor-exec".to_owned()), cursor_exec_resolved: true, ..StreamingBlockState::default() },
    );
    stream.push(AssistantMessageEvent::ToolcallStart { content_index: index, partial: output.clone() });
    stream.push(AssistantMessageEvent::ToolcallEnd {
        content_index: index,
        tool_call: match block {
            ContentBlock::ToolCall(call) => call,
            _ => unreachable!("just pushed a tool call"),
        },
        partial: output.clone(),
    });
}

async fn pair_synthesized_exec_result(
    state: &BlockState,
    on_tool_result: Option<&CursorToolResultHandler>,
    tool_call_id: &str,
    tool_name: &str,
    text: &str,
    is_error: bool,
) {
    let synthesized = ToolResultMessage {
        tool_call_id: tool_call_id.to_owned(),
        tool_name: tool_name.to_owned(),
        content: vec![ContentBlock::text(text)],
        details: None,
        usage: None,
        added_tool_names: None,
        is_error,
        timestamp: now_ms(),
    };
    let Some(sink) = on_tool_result.or(state.on_tool_result.as_ref()) else { return };
    sink(synthesized).await;
}

/// Throttle threshold for mid-stream argument JSON parses.
const STREAMING_ARGS_REPARSE_DELTA: usize = 1024;

/// Exported for tests: drives one Cursor interaction update through the streaming state machine.
pub fn process_interaction_update(
    update: &InteractionUpdate,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    state: &mut BlockState,
    usage_state: &mut UsageState,
) {
    let update_case = update.message.as_ref();
    log("interactionUpdate", update_case.map(interaction_case_name), None);

    match update_case {
        Some(interaction_update::Message::TextDelta(delta_update)) => {
            let delta = delta_update.text.clone();
            let index = match state.current_text_index {
                Some(index) => index,
                None => {
                    let index = output.content.len();
                    output.content.push(ContentBlock::Text(TextContent::default()));
                    state.current_text_index = Some(index);
                    state.current_text_block = Some(TextContent::default());
                    state.streaming.insert(index, StreamingBlockState { block_index: Some(index), ..StreamingBlockState::default() });
                    stream.push(AssistantMessageEvent::TextStart { content_index: index, partial: output.clone() });
                    index
                }
            };
            if let Some(ContentBlock::Text(block)) = output.content.get_mut(index) {
                block.text.push_str(&delta);
                state.current_text_block = Some(block.clone());
            }
            stream.push(AssistantMessageEvent::TextDelta { content_index: index, delta, partial: output.clone() });
        }
        Some(interaction_update::Message::ThinkingDelta(delta_update)) => {
            let delta = delta_update.text.clone();
            let index = match state.current_thinking_index {
                Some(index) => index,
                None => {
                    let index = output.content.len();
                    output.content.push(ContentBlock::Thinking(crate::types::ThinkingContent::default()));
                    state.current_thinking_index = Some(index);
                    state.current_thinking_block = Some(crate::types::ThinkingContent::default());
                    state.streaming.insert(index, StreamingBlockState { block_index: Some(index), ..StreamingBlockState::default() });
                    stream.push(AssistantMessageEvent::ThinkingStart { content_index: index, partial: output.clone() });
                    index
                }
            };
            if let Some(ContentBlock::Thinking(block)) = output.content.get_mut(index) {
                block.thinking.push_str(&delta);
                state.current_thinking_block = Some(block.clone());
            }
            stream.push(AssistantMessageEvent::ThinkingDelta { content_index: index, delta, partial: output.clone() });
        }
        Some(interaction_update::Message::ThinkingCompleted(_)) => {
            end_current_thinking_block(output, stream, state);
        }
        Some(interaction_update::Message::ToolCallStarted(started)) => {
            let tool_call = started.tool_call.clone();
            if let Some(scm_call) = select_connect_scm_call(tool_call.as_ref()) {
                end_current_text_block(output, stream, state);
                end_current_thinking_block(output, stream, state);
                let repository = select_connect_scm_repository(Some(scm_call));
                let mut arguments = Map::new();
                if let Some(repository) = repository {
                    arguments.insert("owner".to_owned(), json!(repository.owner));
                    arguments.insert("repo".to_owned(), json!(repository.repo));
                }
                let id = scm_call
                    .args
                    .as_ref()
                    .map(|args| args.tool_call_id.clone())
                    .filter(|id| !id.is_empty())
                    .or_else(|| Some(started.call_id.clone()).filter(|id| !id.is_empty()))
                    .unwrap_or_else(random_uuid);
                let block = ToolCall {
                    id: id.clone(),
                    name: "connect_scm".to_owned(),
                    arguments,
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                };
                let index = output.content.len();
                output.content.push(ContentBlock::ToolCall(block.clone()));
                state.streaming.insert(
                    index,
                    StreamingBlockState {
                        block_index: Some(index),
                        block_kind: Some("connect-scm".to_owned()),
                        envelope_id: Some(started.call_id.clone()).filter(|id| !id.is_empty()),
                        cursor_exec_resolved: true,
                        ..StreamingBlockState::default()
                    },
                );
                retain_streamed_call(state, index, &block, &started.call_id);
                stream.push(AssistantMessageEvent::ToolcallStart { content_index: index, partial: output.clone() });
                return;
            }
            if is_exec_owned_tool_call(tool_call.as_ref()) {
                end_current_text_block(output, stream, state);
                end_current_thinking_block(output, stream, state);
                log(
                    "exec",
                    Some("streamedToolCallOwnedByExec"),
                    Some(json!({ "case": tool_call.as_ref().and_then(tool_call_case_name) })),
                );
                return;
            }
            end_current_text_block(output, stream, state);
            end_current_thinking_block(output, stream, state);
            let Some(tool_call) = tool_call.as_ref() else { return };
            if let Some(mcp_call) = select_mcp_call(Some(tool_call)) {
                let args = mcp_call.args.clone().unwrap_or_default();
                let id = if args.tool_call_id.is_empty() { random_uuid() } else { args.tool_call_id.clone() };
                let resolved_by_exec = state.resolved_mcp_tool_call_ids.remove(&id);
                if resolved_by_exec
                    && output.content.iter().any(|block| matches!(block, ContentBlock::ToolCall(call) if call.id == id))
                {
                    return;
                }
                let name = if args.tool_name.is_empty() { args.name.clone() } else { args.tool_name.clone() };
                let block = ToolCall {
                    id: id.clone(),
                    name,
                    arguments: Map::new(),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                };
                let index = output.content.len();
                output.content.push(ContentBlock::ToolCall(block.clone()));
                state.streaming.insert(
                    index,
                    StreamingBlockState {
                        block_index: Some(index),
                        partial_json: Some(String::new()),
                        block_kind: Some("mcp".to_owned()),
                        envelope_id: Some(started.call_id.clone()).filter(|id| !id.is_empty()),
                        cursor_exec_resolved: resolved_by_exec,
                        ..StreamingBlockState::default()
                    },
                );
                retain_streamed_call(state, index, &block, &started.call_id);
                stream.push(AssistantMessageEvent::ToolcallStart { content_index: index, partial: output.clone() });
                return;
            }

            let (update_call, read_call) = select_todo_calls(tool_call);
            if update_call.is_some() || read_call.is_some() {
                let call_id =
                    if started.call_id.is_empty() { random_uuid() } else { started.call_id.clone() };
                let block = ToolCall {
                    id: call_id.clone(),
                    name: "todo".to_owned(),
                    arguments: build_todo_display_args(tool_call),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                };
                let index = output.content.len();
                output.content.push(ContentBlock::ToolCall(block.clone()));
                state.streaming.insert(
                    index,
                    StreamingBlockState {
                        block_index: Some(index),
                        block_kind: Some("todo".to_owned()),
                        envelope_id: Some(started.call_id.clone()).filter(|id| !id.is_empty()),
                        cursor_exec_resolved: true,
                        ..StreamingBlockState::default()
                    },
                );
                retain_streamed_call(state, index, &block, &started.call_id);
                stream.push(AssistantMessageEvent::ToolcallStart { content_index: index, partial: output.clone() });
            }
        }
        Some(interaction_update::Message::ToolCallDelta(delta_update)) => {
            // The TS branch reads `argsTextDelta` off the envelope, but the pinned schema
            // declares that field only on `PartialToolCallUpdate`; a `toolCallDelta` frame
            // therefore carries an empty snapshot and applies no delta, exactly as in senpi.
            apply_streamed_args_delta(output, stream, state, &delta_update.call_id, "");
        }
        Some(interaction_update::Message::PartialToolCall(partial)) => {
            apply_streamed_args_delta(output, stream, state, &partial.call_id, &partial.args_text_delta);
        }
        Some(interaction_update::Message::ToolCallCompleted(completed)) => {
            let Some(settled) = resolve_streamed_call(state, &completed.call_id) else { return };
            let tool_call = completed.tool_call.as_ref();
            let kind = state.streaming.get(&settled).and_then(|entry| entry.block_kind.clone());
            match kind.as_deref() {
                Some("mcp") => {
                    let previous_args = match &output.content[settled] {
                        ContentBlock::ToolCall(call) => call.arguments.clone(),
                        _ => Map::new(),
                    };
                    let partial = state.streaming.get(&settled).and_then(|entry| entry.partial_json.clone());
                    let mut arguments = match partial {
                        Some(partial) => parse_streaming_json(Some(&partial)),
                        None => Value::Object(previous_args.clone()),
                    };
                    let decoded = decode_mcp_args_map(select_mcp_call(tool_call).and_then(|call| call.args.as_ref()).map(|args| &args.args));
                    let merged = merge_cursor_mcp_tool_call_args(
                        arguments.as_object(),
                        decoded.as_ref(),
                    );
                    arguments = Value::Object(merged);
                    if let ContentBlock::ToolCall(call) = &mut output.content[settled] {
                        call.arguments = match arguments {
                            Value::Object(map) => map,
                            _ => Map::new(),
                        };
                        if call.name == "task" {
                            let kept = keep_usable_cursor_task_args(
                                Value::Object(previous_args),
                                Value::Object(call.arguments.clone()),
                            );
                            call.arguments = match kept {
                                Value::Object(map) => map,
                                _ => Map::new(),
                            };
                        }
                    }
                }
                Some("connect-scm") => {
                    let scm_call = select_connect_scm_call(tool_call);
                    if let Some(repository) = select_connect_scm_repository(scm_call)
                        && let ContentBlock::ToolCall(call) = &mut output.content[settled]
                    {
                        call.arguments = Map::from_iter([
                            ("owner".to_owned(), json!(repository.owner)),
                            ("repo".to_owned(), json!(repository.repo)),
                        ]);
                    }
                    let (text, is_error) = describe_connect_scm_result(scm_call);
                    if let Some(sink) = state.on_tool_result.as_ref() {
                        let result = ToolResultMessage {
                            tool_call_id: tool_call_id_at(output, settled),
                            tool_name: "connect_scm".to_owned(),
                            content: vec![ContentBlock::text(text)],
                            details: None,
                            usage: None,
                            added_tool_names: None,
                            is_error,
                            timestamp: now_ms(),
                        };
                        let sink = sink.clone();
                        tokio::spawn(async move { sink(result).await });
                    }
                }
                Some("todo") => {
                    let snapshot = tool_call.and_then(extract_todo_snapshot);
                    let error = tool_call.and_then(extract_todo_error);
                    if let Some(snapshot) = &snapshot
                        && let ContentBlock::ToolCall(call) = &mut output.content[settled]
                    {
                        call.arguments = Map::from_iter([
                            (
                                "todos".to_owned(),
                                Value::Array(
                                    snapshot
                                        .todos
                                        .iter()
                                        .map(|todo| json!({ "content": todo.content, "status": todo.status }))
                                        .collect(),
                                ),
                            ),
                            ("merged".to_owned(), Value::Bool(snapshot.merged)),
                        ]);
                    }
                    if let Some(sink) = state.on_tool_result.as_ref() {
                        let result = build_todo_tool_result(&tool_call_id_at(output, settled), snapshot.as_ref(), error.as_deref());
                        let sink = sink.clone();
                        tokio::spawn(async move { sink(result).await });
                    }
                }
                _ => {}
            }
            if let Some(entry) = state.streaming.get_mut(&settled) {
                entry.partial_json = None;
            }
            stream.push(AssistantMessageEvent::ToolcallEnd {
                content_index: settled,
                tool_call: match &output.content[settled] {
                    ContentBlock::ToolCall(call) => call.clone(),
                    _ => return,
                },
                partial: output.clone(),
            });
            release_streamed_call(state, settled);
        }
        Some(interaction_update::Message::TurnEnded(turn_ended)) => {
            output.stop_reason = crate::types::StopReason::Stop;
            apply_billed_turn_ended_usage(turn_ended, output, usage_state);
        }
        Some(interaction_update::Message::TokenDelta(token_delta)) => {
            usage_state.saw_token_delta = true;
            output.usage.output += token_delta.tokens.max(0) as u64;
            output.usage.total_tokens = output.usage.input
                + output.usage.output
                + output.usage.cache_read
                + output.usage.cache_write;
        }
        _ => {}
    }
}

fn tool_call_id_at(output: &AssistantMessage, index: usize) -> String {
    match output.content.get(index) {
        Some(ContentBlock::ToolCall(call)) => call.id.clone(),
        _ => String::new(),
    }
}

/// Cursor's `args_text_delta` is "aggregated args text so far": each delta is a
/// cumulative snapshot. Strip the prefix we already have to recover the new suffix.
fn apply_streamed_args_delta(
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    state: &mut BlockState,
    call_id: &str,
    args_text_delta: &str,
) {
    let Some(target) = resolve_streamed_call(state, call_id) else { return };
    if state.streaming.get(&target).and_then(|entry| entry.block_kind.clone()).as_deref() != Some("mcp") {
        return;
    }
    let snapshot = args_text_delta;
    let current = state.streaming.get(&target).and_then(|entry| entry.partial_json.clone()).unwrap_or_default();
    let chunk = snapshot.strip_prefix(current.as_str()).unwrap_or(snapshot).to_owned();
    if chunk.is_empty() {
        return;
    }
    let next_buffer = format!("{current}{chunk}");
    let last_parse_len = state.streaming.get(&target).and_then(|entry| entry.last_parse_len).unwrap_or(0);
    if let Some(entry) = state.streaming.get_mut(&target) {
        entry.partial_json = Some(next_buffer.clone());
    }
    if next_buffer.len() - last_parse_len >= STREAMING_ARGS_REPARSE_DELTA {
        let parsed = parse_streaming_json(Some(&next_buffer));
        if let (Some(entry), Value::Object(map)) = (state.streaming.get_mut(&target), parsed) {
            if let ContentBlock::ToolCall(call) = &mut output.content[target] {
                call.arguments = map;
            }
            entry.last_parse_len = Some(next_buffer.len());
        }
    }
    stream.push(AssistantMessageEvent::ToolcallDelta {
        content_index: target,
        delta: chunk,
        partial: output.clone(),
    });
}

fn interaction_case_name(message: &interaction_update::Message) -> &'static str {
    match message {
        interaction_update::Message::TextDelta(_) => "textDelta",
        interaction_update::Message::ThinkingDelta(_) => "thinkingDelta",
        interaction_update::Message::ThinkingCompleted(_) => "thinkingCompleted",
        interaction_update::Message::ToolCallStarted(_) => "toolCallStarted",
        interaction_update::Message::ToolCallDelta(_) => "toolCallDelta",
        interaction_update::Message::PartialToolCall(_) => "partialToolCall",
        interaction_update::Message::ToolCallCompleted(_) => "toolCallCompleted",
        interaction_update::Message::TurnEnded(_) => "turnEnded",
        interaction_update::Message::TokenDelta(_) => "tokenDelta",
        interaction_update::Message::Summary(_) => "summary",
        interaction_update::Message::SummaryStarted(_) => "summaryStarted",
        interaction_update::Message::SummaryCompleted(_) => "summaryCompleted",
        interaction_update::Message::Heartbeat(_) => "heartbeat",
        interaction_update::Message::ShellOutputDelta(_) => "shellOutputDelta",
        interaction_update::Message::UserMessageAppended(_) => "userMessageAppended",
        interaction_update::Message::StepStarted(_) => "stepStarted",
        interaction_update::Message::StepCompleted(_) => "stepCompleted",
    }
}

fn tool_call_case_name(tool_call: &PbToolCall) -> Option<&'static str> {
    Some(match tool_call.tool.as_ref()? {
        tool_call::Tool::ShellToolCall(_) => "shellToolCall",
        tool_call::Tool::McpToolCall(_) => "mcpToolCall",
        tool_call::Tool::UpdateTodosToolCall(_) => "updateTodosToolCall",
        tool_call::Tool::ReadTodosToolCall(_) => "readTodosToolCall",
        tool_call::Tool::PiReadToolCall(_) => "piReadToolCall",
        tool_call::Tool::PiBashToolCall(_) => "piBashToolCall",
        tool_call::Tool::PiEditToolCall(_) => "piEditToolCall",
        tool_call::Tool::PiWriteToolCall(_) => "piWriteToolCall",
        tool_call::Tool::PiGrepToolCall(_) => "piGrepToolCall",
        tool_call::Tool::PiFindToolCall(_) => "piFindToolCall",
        tool_call::Tool::PiLsToolCall(_) => "piLsToolCall",
        tool_call::Tool::ConnectScmToolCall(_) => "connectScmToolCall",
        tool_call::Tool::ListMcpResourcesToolCall(_) => "listMcpResourcesToolCall",
        tool_call::Tool::ReadMcpResourceToolCall(_) => "readMcpResourceToolCall",
        _ => "other",
    })
}

/// Cursor's production schema carries the billed token split on `turnEnded`.
fn apply_billed_turn_ended_usage(update: &TurnEndedUpdate, output: &mut AssistantMessage, usage_state: &mut UsageState) {
    let input_tokens = update.input_tokens;
    let output_tokens = update.output_tokens;
    let cache_read_tokens = update.cache_read_tokens;
    let cache_write_tokens = update.cache_write_tokens;
    if input_tokens.is_none() && output_tokens.is_none() && cache_read_tokens.is_none() && cache_write_tokens.is_none() {
        return;
    }
    usage_state.saw_turn_ended_usage = true;
    let usage = &mut output.usage;
    let cache_read = cache_read_tokens.unwrap_or(0).max(0) as u64;
    let cache_write = cache_write_tokens.unwrap_or(0).max(0) as u64;
    let live_used = usage_state.live_used_tokens.unwrap_or(0);
    if live_used > 0 && cache_read > live_used.saturating_mul(3) {
        if let Some(output_tokens) = output_tokens {
            usage.output = output_tokens.max(0) as u64;
        }
        usage.cache_read = 0;
        usage.cache_write = if cache_write <= live_used { cache_write } else { 0 };
        usage.input = live_used.saturating_sub(usage.output + usage.cache_write);
        usage.total_tokens = live_used;
        return;
    }
    usage.cache_read = cache_read;
    usage.cache_write = cache_write;
    usage.input = (input_tokens.unwrap_or(0).max(0) as u64).saturating_sub(usage.cache_read + usage.cache_write);
    if let Some(output_tokens) = output_tokens {
        usage.output = output_tokens.max(0) as u64;
    }
    usage.total_tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
}

/// A checkpoint's `tokenDetails.usedTokens` is the server's live conversation
/// size, sent mid-turn.
fn apply_checkpoint_token_details(
    checkpoint: &ConversationStateStructure,
    output: &mut AssistantMessage,
    usage_state: &mut UsageState,
) {
    if usage_state.saw_turn_ended_usage {
        return;
    }
    let used_tokens = checkpoint.token_details.as_ref().map(|details| details.used_tokens).unwrap_or(0);
    if used_tokens == 0 {
        return;
    }
    usage_state.live_used_tokens = Some(used_tokens as u64);
    let usage = &mut output.usage;
    usage.input = (used_tokens as u64).saturating_sub(usage.output + usage.cache_read + usage.cache_write);
    usage.total_tokens = usage.input + usage.output + usage.cache_read + usage.cache_write;
}

/// Local tools Cursor already drives natively over the exec channel.
const CURSOR_NATIVE_TOOL_NAMES: &[&str] = &["bash", "read", "write", "delete", "ls", "grep", "todo"];

const CURSOR_UNSUPPORTED_SCHEMA_KEYS: &[&str] = &["oneOf", "anyOf", "allOf"];

pub fn sanitize_cursor_tool_schema(schema: &Value) -> Value {
    match schema {
        Value::Array(items) => Value::Array(items.iter().map(sanitize_cursor_tool_schema).collect()),
        Value::Object(entries) => {
            let mut sanitized = Map::new();
            for (key, value) in entries {
                if CURSOR_UNSUPPORTED_SCHEMA_KEYS.contains(&key.as_str()) {
                    continue;
                }
                sanitized.insert(key.clone(), sanitize_cursor_tool_schema(value));
            }
            Value::Object(sanitized)
        }
        other => other.clone(),
    }
}

pub fn build_mcp_tool_definitions(tools: Option<&[Tool]>) -> Vec<McpToolDefinition> {
    let Some(tools) = tools.filter(|tools| !tools.is_empty()) else { return Vec::new() };
    let advertised: Vec<&Tool> =
        tools.iter().filter(|tool| !CURSOR_NATIVE_TOOL_NAMES.contains(&tool.name.as_str())).collect();
    if advertised.is_empty() {
        return Vec::new();
    }
    advertised
        .into_iter()
        .map(|tool| {
            let json_schema = sanitize_cursor_tool_schema(&tool.parameters);
            let schema_value = if json_schema.is_object() {
                json_schema
            } else {
                json!({ "type": "object", "properties": {}, "required": [] })
            };
            McpToolDefinition {
                name: tool.name.clone(),
                provider_identifier: "pi-agent".to_owned(),
                tool_name: tool.name.clone(),
                description: tool.description.clone(),
                input_schema: value_to_bytes(&schema_value).into(),
                ..McpToolDefinition::default()
            }
        })
        .collect()
}

fn mark_cursor_exec_resolved(state: &mut BlockState, index: usize) {
    if let Some(entry) = state.streaming.get_mut(&index) {
        entry.cursor_exec_resolved = true;
    }
}

fn retain_streamed_call(state: &mut BlockState, index: usize, block: &ToolCall, envelope_id: &str) {
    if let Some(entry) = state.streaming.get_mut(&index) {
        entry.envelope_id = Some(envelope_id.to_owned()).filter(|id| !id.is_empty());
    }
    if !envelope_id.is_empty() {
        state.open_tool_calls.insert(envelope_id.to_owned(), (index, block.id.clone()));
    }
    state.current_tool_call = Some(index);
}

fn resolve_streamed_call(state: &BlockState, envelope_id: &str) -> Option<usize> {
    if envelope_id.is_empty() {
        return state.current_tool_call;
    }
    if let Some((index, _)) = state.open_tool_calls.get(envelope_id) {
        return Some(*index);
    }
    match state.current_tool_call {
        Some(index)
            if state.streaming.get(&index).and_then(|entry| entry.envelope_id.as_ref()).is_none() =>
        {
            Some(index)
        }
        _ => None,
    }
}

fn release_streamed_call(state: &mut BlockState, index: usize) {
    let envelope_id = state.streaming.get(&index).and_then(|entry| entry.envelope_id.clone());
    if let Some(envelope_id) = envelope_id {
        state.open_tool_calls.remove(&envelope_id);
    }
    if state.current_tool_call == Some(index) {
        state.current_tool_call = None;
    }
}

/// Close every tool-call block still open when the stream ends. Exported for tests.
pub fn flush_open_tool_calls(output: &mut AssistantMessage, stream: &AssistantMessageEventStream, state: &mut BlockState) {
    let mut open_blocks: Vec<usize> = state.open_tool_calls.values().map(|(index, _)| *index).collect();
    if let Some(current) = state.current_tool_call
        && !open_blocks.contains(&current)
    {
        open_blocks.push(current);
    }
    for index in open_blocks {
        let partial_json = state.streaming.get(&index).and_then(|entry| entry.partial_json.clone());
        if let Some(partial_json) = partial_json {
            let parsed = parse_streaming_json(Some(&partial_json));
            if let (Value::Object(map), Some(ContentBlock::ToolCall(call))) = (parsed, output.content.get_mut(index)) {
                call.arguments = map;
            }
            if let Some(entry) = state.streaming.get_mut(&index) {
                entry.partial_json = None;
            }
        }
        let kind = state.streaming.get(&index).and_then(|entry| entry.block_kind.clone());
        if matches!(kind.as_deref(), Some("connect-scm") | Some("todo"))
            && let Some(sink) = state.on_tool_result.as_ref()
        {
            let result = ToolResultMessage {
                tool_call_id: tool_call_id_at(output, index),
                tool_name: match output.content.get(index) {
                    Some(ContentBlock::ToolCall(call)) => call.name.clone(),
                    _ => String::new(),
                },
                content: vec![ContentBlock::text(
                    "The connection to Cursor closed before this call completed.",
                )],
                details: None,
                usage: None,
                added_tool_names: None,
                is_error: true,
                timestamp: now_ms(),
            };
            let sink = sink.clone();
            tokio::spawn(async move { sink(result).await });
        }
        if let Some(ContentBlock::ToolCall(call)) = output.content.get(index) {
            stream.push(AssistantMessageEvent::ToolcallEnd {
                content_index: index,
                tool_call: call.clone(),
                partial: output.clone(),
            });
        }
    }
    state.open_tool_calls.clear();
    state.current_tool_call = None;
}

pub struct BlockState {
    pub current_text_block: Option<TextContent>,
    pub current_text_index: Option<usize>,
    pub current_thinking_block: Option<crate::types::ThinkingContent>,
    pub current_thinking_index: Option<usize>,
    pub current_tool_call: Option<usize>,
    pub open_tool_calls: HashMap<String, (usize, String)>,
    pub resolved_mcp_tool_call_ids: HashSet<String>,
    pub streaming: HashMap<usize, StreamingBlockState>,
    pub on_tool_result: Option<CursorToolResultHandler>,
}

#[derive(Default)]
pub struct UsageState {
    pub saw_token_delta: bool,
    pub saw_turn_ended_usage: bool,
    pub live_used_tokens: Option<u64>,
}

// ---------------------------------------------------------------------------
// History serialization
// ---------------------------------------------------------------------------

fn extract_user_message_text(message: &Message) -> String {
    let Message::User(user) = message else { return String::new() };
    match &user.content {
        crate::types::UserContent::Text(text) => text.trim().to_owned(),
        crate::types::UserContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
            .trim()
            .to_owned(),
    }
}

fn has_user_message_images(message: &Message) -> bool {
    matches!(&message, Message::User(user) if matches!(&user.content, crate::types::UserContent::Blocks(blocks) if blocks.iter().any(|block| matches!(block, ContentBlock::Image(_)))))
}

fn build_cursor_root_prompt_content(content: &crate::types::UserContent) -> Vec<Value> {
    match content {
        crate::types::UserContent::Text(text) => {
            let text = text.trim();
            if text.is_empty() {
                Vec::new()
            } else {
                vec![json!({ "type": "text", "text": text })]
            }
        }
        crate::types::UserContent::Blocks(blocks) => {
            let mut parts = Vec::new();
            for block in blocks {
                match block {
                    ContentBlock::Text(text) => {
                        let text = text.text.trim();
                        if !text.is_empty() {
                            parts.push(json!({ "type": "text", "text": text }));
                        }
                    }
                    ContentBlock::Image(image) => parts.push(json!({
                        "type": "image",
                        "image": format!("data:{};base64,{}", image.mime_type, image.data),
                        "mediaType": image.mime_type,
                    })),
                    ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                        parts.push(json!({
                            "type": "image",
                            "image": "data:undefined;base64,undefined",
                            "mediaType": Value::Null,
                        }));
                    }
                }
            }
            parts
        }
    }
}

fn sha256_hex(data: &[u8]) -> String {
    use sha2::Digest as _;
    let mut hasher = sha2::Sha256::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

fn cursor_user_content_key(content: &crate::types::UserContent) -> String {
    match content {
        crate::types::UserContent::Text(text) => text.trim().to_owned(),
        crate::types::UserContent::Blocks(blocks) => {
            let mut data = String::new();
            for block in blocks {
                match block {
                    ContentBlock::Text(text) => {
                        data.push_str("text");
                        data.push_str(&text.text);
                    }
                    ContentBlock::Image(image) => {
                        data.push_str("image");
                        data.push_str(&image.mime_type);
                        data.push_str(&image.data);
                    }
                    ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                        data.push_str("image");
                        data.push_str("undefined");
                        data.push_str("undefined");
                    }
                }
            }
            sha256_hex(data.as_bytes())
        }
    }
}

fn build_cursor_assistant_content(message: &AssistantMessage) -> Vec<Value> {
    let mut content = Vec::new();
    for item in &message.content {
        match item {
            ContentBlock::Text(text) => {
                if !text.text.is_empty() {
                    content.push(json!({ "type": "text", "text": text.text }));
                }
            }
            ContentBlock::ToolCall(call) => content.push(json!({
                "type": "tool-call",
                "toolCallId": call.id,
                "toolName": call.name,
                "args": call.arguments,
            })),
            ContentBlock::Thinking(_) | ContentBlock::Image(_) | ContentBlock::ProviderNative(_) => {}
        }
    }
    content
}

pub fn build_cursor_system_prompt_jsons(system_prompt: Option<&str>, model_id: Option<&str>) -> Vec<String> {
    let trimmed = system_prompt.map(str::trim).filter(|prompt| !prompt.is_empty());
    let host = vec![match trimmed {
        Some(prompt) => json!({ "role": "system", "content": prompt }).to_string(),
        None => json!({ "role": "system", "content": "You are a helpful assistant." }).to_string(),
    }];
    match model_id {
        Some(model_id) if is_cursor_composer_model(model_id) => {
            let mut result = vec![json!({ "role": "system", "content": CURSOR_COMPOSER_PROMPT }).to_string()];
            result.extend(host);
            result
        }
        _ => host,
    }
}

fn build_root_prompt_messages_json(
    messages: &[Message],
    system_prompt_ids: &[Vec<u8>],
    blob_store: &Arc<Mutex<ConversationBlobStore>>,
    active_user_message_index: Option<usize>,
) -> Vec<Vec<u8>> {
    let mut entries: Vec<Vec<u8>> = system_prompt_ids.to_vec();
    for (index, message) in messages.iter().enumerate() {
        if Some(index) == active_user_message_index {
            break;
        }
        match message {
            Message::User(user) => {
                let content = build_cursor_root_prompt_content(&user.content);
                if content.is_empty() {
                    continue;
                }
                entries.push(store_cursor_blob(
                    blob_store,
                    json!({ "role": "user", "content": content }).to_string().into_bytes(),
                ));
            }
            Message::Assistant(assistant) => {
                let content = build_cursor_assistant_content(assistant);
                if content.is_empty() {
                    continue;
                }
                entries.push(store_cursor_blob(
                    blob_store,
                    json!({ "role": "assistant", "content": content }).to_string().into_bytes(),
                ));
            }
            Message::ToolResult(result) => {
                let mut item = json!({
                    "type": "tool-result",
                    "toolName": result.tool_name,
                    "toolCallId": result.tool_call_id,
                    "result": tool_result_to_text(result),
                });
                if result.is_error {
                    item["isError"] = Value::Bool(true);
                }
                entries.push(store_cursor_blob(
                    blob_store,
                    json!({ "role": "tool", "id": result.tool_call_id, "content": [item] }).to_string().into_bytes(),
                ));
            }
            Message::ConfigurationUpdate(_) => {}
        }
    }
    entries
}

fn is_json_value(value: &Value) -> bool {
    match value {
        Value::Null | Value::String(_) | Value::Bool(_) => true,
        Value::Number(number) => number.as_f64().is_some_and(f64::is_finite),
        Value::Array(items) => items.iter().all(is_json_value),
        Value::Object(entries) => entries.values().all(is_json_value),
    }
}

fn encode_cursor_mcp_arguments(tool_call: &ToolCall) -> Result<HashMap<String, Bytes>, String> {
    let mut encoded = HashMap::new();
    for (name, value) in &tool_call.arguments {
        if !is_json_value(value) {
            return Err(format!("Cursor tool argument {}.{} is not JSON-serializable", tool_call.name, name));
        }
        encoded.insert(name.clone(), value_to_bytes(value).into());
    }
    Ok(encoded)
}

fn create_cursor_mcp_result(result: &ToolResultMessage) -> McpToolResult {
    if result.is_error {
        return McpToolResult {
            result: Some(mcp_tool_result::Result::Error(McpToolError {
                error: tool_result_to_text(result),
                ..McpToolError::default()
            })),
        };
    }
    let content = result
        .content
        .iter()
        .map(|item| match item {
            ContentBlock::Image(image) => McpToolResultContentItem {
                content: Some(mcp_tool_result_content_item::Content::Image(McpImageContent {
                    data: base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap_or_default().into(),
                    mime_type: image.mime_type.clone(),
                })),
            },
            ContentBlock::Text(text) => McpToolResultContentItem {
                content: Some(mcp_tool_result_content_item::Content::Text(McpTextContent {
                    text: text.text.clone(),
                    ..McpTextContent::default()
                })),
            },
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::ProviderNative(_) => {
                McpToolResultContentItem {
                    content: Some(mcp_tool_result_content_item::Content::Text(McpTextContent {
                        text: "[undefined image]".to_owned(),
                        ..McpTextContent::default()
                    })),
                }
            }
        })
        .collect();
    McpToolResult { result: Some(mcp_tool_result::Result::Success(McpSuccess { content, is_error: false })) }
}

fn create_cursor_tool_call_step(
    tool_call: &ToolCall,
    result: Option<&ToolResultMessage>,
) -> Result<ConversationStep, String> {
    let mcp_call = McpToolCall {
        args: Some(McpArgs {
            name: tool_call.name.clone(),
            args: encode_cursor_mcp_arguments(tool_call)?,
            tool_call_id: tool_call.id.clone(),
            provider_identifier: "pi-agent".to_owned(),
            tool_name: tool_call.name.clone(),
            ..McpArgs::default()
        }),
        result: result.map(create_cursor_mcp_result),
        ..McpToolCall::default()
    };
    Ok(ConversationStep {
        message: Some(conversation_step::Message::ToolCall(PbToolCall {
            tool: Some(tool_call::Tool::McpToolCall(mcp_call)),
            tool_call_id: Some(tool_call.id.clone()),
        })),
    })
}

fn build_conversation_turns(
    messages: &[Message],
    blob_store: &Arc<Mutex<ConversationBlobStore>>,
    active_user_message_index: Option<usize>,
) -> Result<Vec<Vec<u8>>, String> {
    let mut turns = Vec::new();
    let history_end = active_user_message_index.unwrap_or(messages.len());
    let mut tool_results: HashMap<String, &ToolResultMessage> = HashMap::new();
    let mut paired_tool_call_ids: HashSet<String> = HashSet::new();
    for message in messages.iter().take(history_end) {
        match message {
            Message::ToolResult(result) => {
                tool_results.insert(result.tool_call_id.clone(), result);
            }
            Message::Assistant(assistant) => {
                for item in &assistant.content {
                    if let ContentBlock::ToolCall(call) = item {
                        paired_tool_call_ids.insert(call.id.clone());
                    }
                }
            }
            Message::User(_) | Message::ConfigurationUpdate(_) => {}
        }
    }

    let mut index = 0;
    while index < messages.len() {
        let Message::User(user) = &messages[index] else {
            index += 1;
            continue;
        };
        if Some(index) == active_user_message_index {
            break;
        }
        let user_text = extract_user_message_text(&messages[index]);
        if user_text.is_empty() && !has_user_message_images(&messages[index]) {
            index += 1;
            continue;
        }
        let user_message = create_cursor_user_message(
            &user.content,
            &user_text,
            deterministic_uuid(&format!("u:{}:{}", turns.len(), cursor_user_content_key(&user.content))),
        );
        let user_message_blob_id = store_cursor_blob(blob_store, user_message.encode_to_vec());
        let mut step_blob_ids: Vec<Vec<u8>> = Vec::new();
        index += 1;
        while index < messages.len() && !matches!(messages[index], Message::User(_)) {
            match &messages[index] {
                Message::Assistant(assistant) => {
                    for item in &assistant.content {
                        let step = match item {
                            ContentBlock::Text(text) => {
                                if text.text.is_empty() {
                                    continue;
                                }
                                ConversationStep {
                                    message: Some(conversation_step::Message::AssistantMessage(
                                        super::cursor_agent::r#gen::agent_pb::AssistantMessage {
                                            text: text.text.clone(),
                                        },
                                    )),
                                }
                            }
                            ContentBlock::Thinking(_) => continue,
                            ContentBlock::ToolCall(call) => {
                                create_cursor_tool_call_step(call, tool_results.get(&call.id).copied())?
                            }
                            ContentBlock::Image(_) | ContentBlock::ProviderNative(_) => continue,
                        };
                        step_blob_ids.push(store_cursor_blob(blob_store, step.encode_to_vec()));
                    }
                }
                Message::ToolResult(result) if !paired_tool_call_ids.contains(&result.tool_call_id) => {
                    let text = tool_result_to_text(result);
                    if !text.is_empty() {
                        let prefix = if result.is_error { "[Tool Error]" } else { "[Tool Result]" };
                        let step = ConversationStep {
                            message: Some(conversation_step::Message::AssistantMessage(
                                super::cursor_agent::r#gen::agent_pb::AssistantMessage {
                                    text: format!("{prefix}\n{text}"),
                                },
                            )),
                        };
                        step_blob_ids.push(store_cursor_blob(blob_store, step.encode_to_vec()));
                    }
                }
                Message::User(_) | Message::ToolResult(_) | Message::ConfigurationUpdate(_) => {}
            }
            index += 1;
        }
        let agent_turn = AgentConversationTurnStructure {
            user_message: user_message_blob_id.into(),
            steps: step_blob_ids.into_iter().map(Into::into).collect(),
            request_id: None,
        };
        let turn = ConversationTurnStructure {
            turn: Some(conversation_turn_structure::Turn::AgentConversationTurn(agent_turn)),
        };
        turns.push(store_cursor_blob(blob_store, turn.encode_to_vec()));
    }
    Ok(turns)
}

/// Exported for tests: decodes Cursor history blobs built from conversation messages.
pub fn build_cursor_history_for_test(
    messages: &[Message],
    active_user_message_index: Option<usize>,
) -> Result<crate::api::cursor_agent::measure::CursorHistoryForTest, String> {
    crate::api::cursor_agent::measure::build_cursor_history_for_test(messages, active_user_message_index)
}

fn create_cursor_user_message(
    content: &crate::types::UserContent,
    text: &str,
    message_id: String,
) -> PbUserMessage {
    let images = match content {
        crate::types::UserContent::Text(_) => Vec::new(),
        crate::types::UserContent::Blocks(blocks) => extract_images(blocks),
    };
    PbUserMessage {
        text: text.to_owned(),
        message_id,
        selected_context: if images.is_empty() {
            None
        } else {
            Some(SelectedContext { selected_images: images, ..SelectedContext::default() })
        },
        ..PbUserMessage::default()
    }
}

fn extract_images(blocks: &[ContentBlock]) -> Vec<SelectedImage> {
    blocks
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Image(image) => Some(SelectedImage {
                uuid: random_uuid(),
                mime_type: image.mime_type.clone(),
                data_or_blob_id: Some(super::cursor_agent::r#gen::agent_pb::selected_image::DataOrBlobId::Data(
                    base64::engine::general_purpose::STANDARD.decode(&image.data).unwrap_or_default().into(),
                )),
                ..SelectedImage::default()
            }),
            _ => None,
        })
        .collect()
}

fn has_images(content: &[ContentBlock]) -> bool {
    content.iter().any(|item| matches!(item, ContentBlock::Image(_)))
}

fn extract_text(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text(text) => Some(text.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

struct GrpcRequestState {
    conversation_id: String,
    blob_store: Arc<Mutex<ConversationBlobStore>>,
    conversation_state: Option<ConversationStateStructure>,
    force_resume_action: bool,
    pinned_requested_model: Option<RequestedModel>,
    pinned_model_details: Option<ModelDetails>,
}

struct GrpcRequestOutput {
    request_bytes: Vec<u8>,
    conversation_state: ConversationStateStructure,
    requested_model: RequestedModel,
    model_details: ModelDetails,
}

fn build_grpc_request(
    model: &Model,
    context: &Context,
    options: Option<&CursorAgentOptions>,
    state: GrpcRequestState,
) -> Result<GrpcRequestOutput, String> {
    let blob_store = state.blob_store.clone();
    let system_prompt_ids: Vec<Vec<u8>> = build_cursor_system_prompt_jsons(context.system_prompt.as_deref(), Some(&model.id))
        .into_iter()
        .map(|json| store_cursor_blob(&blob_store, json.into_bytes()))
        .collect();

    let active_user_message_index = context.messages.len().checked_sub(1);
    let active_user_message = active_user_message_index
        .and_then(|index| context.messages.get(index))
        .and_then(|message| match message {
            Message::User(user) => Some(user),
            _ => None,
        });
    let mut user_content: Option<&crate::types::UserContent> = None;
    let mut user_text = String::new();
    let mut user_has_images = false;
    if let Some(active_user_message) = active_user_message {
        user_content = Some(&active_user_message.content);
        match &active_user_message.content {
            crate::types::UserContent::Text(text) => user_text = text.trim().to_owned(),
            crate::types::UserContent::Blocks(blocks) => {
                user_text = extract_text(blocks);
                user_has_images = has_images(blocks);
            }
        }
    }

    let send_user_message =
        !state.force_resume_action && user_content.is_some_and(|_| !user_text.trim().is_empty() || user_has_images);
    let action = ConversationAction {
        action: Some(if send_user_message {
            conversation_action::Action::UserMessageAction(UserMessageAction {
                user_message: Some(create_cursor_user_message(
                    user_content.expect("checked above"),
                    &user_text,
                    random_uuid(),
                )),
                ..UserMessageAction::default()
            })
        } else {
            conversation_action::Action::ResumeAction(ResumeAction::default())
        }),
    };

    let turns = build_conversation_turns(
        &context.messages,
        &blob_store,
        if active_user_message.is_some() { active_user_message_index } else { None },
    )?;
    let root_prompt_messages_json = build_root_prompt_messages_json(
        &context.messages,
        &system_prompt_ids,
        &blob_store,
        if active_user_message.is_some() { active_user_message_index } else { None },
    );

    let cached_prompt_head: Vec<Vec<u8>> = state
        .conversation_state
        .as_ref()
        .map(|cached| {
            cached.root_prompt_messages_json.iter().take(system_prompt_ids.len()).map(|bytes| bytes.to_vec()).collect()
        })
        .unwrap_or_default();
    let has_matching_prompt = cached_prompt_head.len() == system_prompt_ids.len()
        && system_prompt_ids.iter().zip(cached_prompt_head.iter()).all(|(id, cached)| id == cached);
    let base_state = match &state.conversation_state {
        Some(cached) if has_matching_prompt => cached.clone(),
        _ => ConversationStateStructure {
            root_prompt_messages_json: system_prompt_ids.iter().map(|id| id.clone().into()).collect(),
            turns: Vec::new(),
            todos: Vec::new(),
            pending_tool_calls: Vec::new(),
            previous_workspace_uris: Vec::new(),
            file_states: HashMap::new(),
            file_states_v2: HashMap::new(),
            summary_archives: Vec::new(),
            turn_timings: Vec::new(),
            subagent_states: HashMap::new(),
            self_summary_count: 0,
            read_paths: Vec::new(),
            ..ConversationStateStructure::default()
        },
    };

    let conversation_state = ConversationStateStructure {
        root_prompt_messages_json: root_prompt_messages_json.iter().map(|id| id.clone().into()).collect(),
        turns: turns.iter().map(|id| id.clone().into()).collect(),
        ..base_state
    };

    let requested_model = state
        .pinned_requested_model
        .clone()
        .unwrap_or_else(|| build_requested_model(model, options.and_then(|options| options.thinking_selection.as_ref())));
    let wire_model_id = requested_model.model_id.clone();
    let cursor_max_mode = model.compat.as_ref().is_some_and(|compat| compat.cursor_agent().cursor_max_mode == Some(true));
    let model_details = state.pinned_model_details.clone().unwrap_or_else(|| ModelDetails {
        model_id: wire_model_id.clone(),
        display_model_id: model.id.clone(),
        display_name: model.name.clone(),
        max_mode: cursor_max_mode.then_some(true),
        ..ModelDetails::default()
    });

    let mut run_request = AgentRunRequest {
        conversation_state: Some(conversation_state.clone()),
        action: Some(action),
        model_details: Some(model_details.clone()),
        requested_model: Some(requested_model.clone()),
        conversation_id: Some(state.conversation_id.clone()),
        ..AgentRunRequest::default()
    };
    if let Some(custom_system_prompt) = options.and_then(|options| options.custom_system_prompt.clone()) {
        run_request.custom_system_prompt = Some(custom_system_prompt);
    }
    if let Some(on_payload) = options.and_then(|options| options.base.request.on_payload.as_ref()) {
        let _ = on_payload(&run_request_payload(&run_request), model, None);
    }

    let client_message = AgentClientMessage {
        message: Some(agent_client_message::Message::RunRequest(run_request)),
    };
    let request_bytes = client_message.encode_to_vec();
    log(
        "info",
        Some("builtRunRequest"),
        Some(json!({ "bytes": request_bytes.len(), "tools": context.tools.as_ref().map(Vec::len).unwrap_or(0) })),
    );

    Ok(GrpcRequestOutput { request_bytes, conversation_state, requested_model, model_details })
}

/// JSON view of the run request handed to the onPayload hook. The TS hook
/// receives the live protobuf message; the Rust port hands it the same fields
/// as JSON, which is what every hook in senpi reads.
fn run_request_payload(run_request: &AgentRunRequest) -> Value {
    let mut object = Map::new();
    if let Some(state) = &run_request.conversation_state {
        object.insert(
            "conversationState".to_owned(),
            json!({
                "rootPromptMessagesJson": state.root_prompt_messages_json.len(),
                "turns": state.turns.len(),
                "todos": state.todos.len(),
                "pendingToolCalls": state.pending_tool_calls.len(),
            }),
        );
    }
    if let Some(model_details) = &run_request.model_details {
        object.insert(
            "modelDetails".to_owned(),
            json!({
                "modelId": model_details.model_id,
                "displayModelId": model_details.display_model_id,
                "displayName": model_details.display_name,
                "maxMode": model_details.max_mode == Some(true),
            }),
        );
    }
    if let Some(requested_model) = &run_request.requested_model {
        object.insert(
            "requestedModel".to_owned(),
            json!({
                "modelId": requested_model.model_id,
                "maxMode": requested_model.max_mode,
                "parameters": requested_model
                    .parameters
                    .iter()
                    .map(|parameter| json!({ "id": parameter.id, "value": parameter.value }))
                    .collect::<Vec<_>>(),
            }),
        );
    }
    if let Some(conversation_id) = &run_request.conversation_id {
        object.insert("conversationId".to_owned(), json!(conversation_id));
    }
    if let Some(custom_system_prompt) = &run_request.custom_system_prompt {
        object.insert("customSystemPrompt".to_owned(), json!(custom_system_prompt));
    }
    Value::Object(object)
}

fn deterministic_uuid(seed: &str) -> String {
    let hex = sha256_hex(seed.as_bytes());
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

/// A writable Connect frame channel over one HTTP/2 stream.
#[derive(Clone)]
pub struct CursorTransport {
    send: Arc<tokio::sync::Mutex<Option<h2::SendStream<Bytes>>>>,
    closed: Arc<AtomicBool>,
}

impl CursorTransport {
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub async fn write_frame(&self, bytes: Vec<u8>) {
        if self.is_closed() {
            return;
        }
        let mut send = self.send.lock().await;
        if let Some(stream) = send.as_mut() {
            let _ = stream.send_data(Bytes::from(bytes), false);
        }
    }

}

fn parse_base_url(base_url: &str) -> Result<(String, String, bool), String> {
    let url = url::Url::parse(base_url).map_err(|error| error.to_string())?;
    let scheme = url.scheme().to_owned();
    let host = url.host_str().ok_or_else(|| "base url has no host".to_owned())?.to_owned();
    let port = url.port_or_known_default().unwrap_or(if scheme == "https" { 443 } else { 80 });
    Ok((format!("{host}:{port}"), host, scheme == "https"))
}

async fn open_h2(
    base_url: &str,
) -> Result<(h2::client::SendRequest<Bytes>, tokio::task::JoinHandle<()>), String> {
    let (authority, host, tls) = parse_base_url(base_url)?;
    let tcp = tokio::net::TcpStream::connect(&authority).await.map_err(|error| error.to_string())?;
    let _ = tcp.set_nodelay(true);

    if tls {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = rustls_pki_types::ServerName::try_from(host.clone())
            .map_err(|error| error.to_string())?;
        let stream = connector.connect(server_name, tcp).await.map_err(|error| error.to_string())?;
        handshake(stream).await
    } else {
        handshake(tcp).await
    }
}

async fn handshake<S>(stream: S) -> Result<(h2::client::SendRequest<Bytes>, tokio::task::JoinHandle<()>), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (send_request, connection) = h2::client::handshake(stream).await.map_err(|error| error.to_string())?;
    let task = tokio::spawn(async move {
        let _ = connection.await;
    });
    Ok((send_request, task))
}

fn build_run_request_headers(
    base_url: &str,
    api_key: &str,
    caller_headers: &BTreeMap<String, String>,
) -> Result<http::Request<()>, String> {
    let _ = base_url;
    let mut builder = http::Request::builder()
        .method("POST")
        .uri("/agent.v1.AgentService/Run")
        .header("content-type", "application/connect+proto")
        .header("connect-protocol-version", "1")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {api_key}"))
        .header("x-ghost-mode", "true")
        .header("x-cursor-client-version", CURSOR_CLIENT_VERSION)
        .header("x-cursor-client-type", "cli")
        .header("x-request-id", random_uuid());
    for (name, value) in caller_headers {
        builder = builder.header(name.as_str(), value.as_str());
    }
    builder.body(()).map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Exec dispatch
// ---------------------------------------------------------------------------

pub async fn send_exec_client_message(
    transport: &CursorTransport,
    exec_msg: &ExecServerMessage,
    message: exec_client_message::Message,
) {
    let case = exec_client_case_name(&message);
    let exec_client_message = ExecClientMessage {
        id: exec_msg.id,
        exec_id: exec_msg.exec_id.clone(),
        message: Some(message),
        ..ExecClientMessage::default()
    };
    let client_message = AgentClientMessage {
        message: Some(agent_client_message::Message::ExecClientMessage(exec_client_message)),
    };
    transport.write_frame(frame_connect_message(&client_message.encode_to_vec(), 0)).await;
    log("execClientMessage", Some(case), None);
}

fn exec_client_case_name(message: &exec_client_message::Message) -> &'static str {
    match message {
        exec_client_message::Message::ReadResult(_) => "readResult",
        exec_client_message::Message::LsResult(_) => "lsResult",
        exec_client_message::Message::GrepResult(_) => "grepResult",
        exec_client_message::Message::WriteResult(_) => "writeResult",
        exec_client_message::Message::DeleteResult(_) => "deleteResult",
        exec_client_message::Message::ShellResult(_) => "shellResult",
        exec_client_message::Message::DiagnosticsResult(_) => "diagnosticsResult",
        exec_client_message::Message::McpResult(_) => "mcpResult",
        exec_client_message::Message::RequestContextResult(_) => "requestContextResult",
        exec_client_message::Message::PiReadResult(_) => "piReadResult",
        exec_client_message::Message::PiBashResult(_) => "piBashResult",
        exec_client_message::Message::PiEditResult(_) => "piEditResult",
        exec_client_message::Message::PiWriteResult(_) => "piWriteResult",
        exec_client_message::Message::PiGrepResult(_) => "piGrepResult",
        exec_client_message::Message::PiFindResult(_) => "piFindResult",
        exec_client_message::Message::PiLsResult(_) => "piLsResult",
        exec_client_message::Message::ShellStream(_) => "shellStream",
        _ => "execClientMessage",
    }
}

/// Fail one exec frame in band.
async fn send_exec_client_throw(
    transport: &CursorTransport,
    exec_msg: &ExecServerMessage,
    error: &str,
    error_code: Option<&str>,
) {
    let control_message = ExecClientControlMessage {
        message: Some(exec_client_control_message::Message::Throw(ExecClientThrow {
            id: exec_msg.id,
            error: error.to_owned(),
            error_code: error_code.map(str::to_owned),
            ..ExecClientThrow::default()
        })),
    };
    let client_message = AgentClientMessage {
        message: Some(agent_client_message::Message::ExecClientControlMessage(control_message)),
    };
    transport.write_frame(frame_connect_message(&client_message.encode_to_vec(), 0)).await;
    log("execClientControl", Some("throw"), Some(json!({ "id": exec_msg.id, "execId": exec_msg.exec_id, "error": error, "errorCode": error_code })));
}

async fn send_exec_client_stream_close(transport: &CursorTransport, exec_msg: &ExecServerMessage) {
    let close_message = ExecClientControlMessage {
        message: Some(exec_client_control_message::Message::StreamClose(ExecClientStreamClose { id: exec_msg.id })),
    };
    let client_message = AgentClientMessage {
        message: Some(agent_client_message::Message::ExecClientControlMessage(close_message)),
    };
    transport.write_frame(frame_connect_message(&client_message.encode_to_vec(), 0)).await;
    log("execClientControl", Some("streamClose"), Some(json!({ "id": exec_msg.id, "execId": exec_msg.exec_id })));
}

fn arm_cursor_exec_heartbeat(transport: &CursorTransport, exec_msg: &ExecServerMessage) -> tokio::task::JoinHandle<()> {
    let transport = transport.clone();
    let exec_msg = exec_msg.clone();
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_millis(EXEC_HEARTBEAT_INTERVAL_MS));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            if transport.is_closed() {
                return;
            }
            let control_message = ExecClientControlMessage {
                message: Some(exec_client_control_message::Message::Heartbeat(ExecClientHeartbeat { id: exec_msg.id })),
            };
            let client_message = AgentClientMessage {
                message: Some(agent_client_message::Message::ExecClientControlMessage(control_message)),
            };
            transport.write_frame(frame_connect_message(&client_message.encode_to_vec(), 0)).await;
            log("execClientControl", Some("heartbeat"), Some(json!({ "id": exec_msg.id, "execId": exec_msg.exec_id })));
        }
    })
}

/// Exported for tests: dispatches one exec frame onto its handler.
pub async fn resolve_exec_handler<TArgs, TResult>(
    args: TArgs,
    handler: Option<&Arc<dyn Fn(TArgs) -> crate::api::cursor_agent::types::ExecFuture<TResult> + Send + Sync>>,
    on_tool_result: Option<&CursorToolResultHandler>,
    build_from_tool_result: impl Fn(&ToolResultMessage) -> TResult,
    build_rejected: impl Fn(&str) -> TResult,
    _build_error: impl Fn(&str) -> TResult,
    pairing: Option<&CursorExecPairing>,
) -> (TResult, Option<ToolResultMessage>)
where
    TArgs: Send + 'static,
    TResult: DescribeExecResult + Send + 'static,
{
    let pair = |text: String, is_error: bool| {
        let pairing = pairing.cloned();
        async move {
        let pairing = pairing?;
        let synthesized = ToolResultMessage {
            tool_call_id: pairing.tool_call_id.clone(),
            tool_name: pairing.tool_name.clone(),
            content: vec![ContentBlock::text(text)],
            details: None,
            usage: None,
            added_tool_names: None,
            is_error,
            timestamp: now_ms(),
        };
        apply_tool_result_handler(Some(synthesized), on_tool_result).await
        }
    };

    let Some(handler) = handler else {
        let reason = "Tool not available";
        let tool_result = pair(reason.to_owned(), true).await;
        return (build_rejected(reason), tool_result);
    };

    let handler_result = handler(args).await;
    let (exec_result, tool_result) = split_exec_handler_result(handler_result);
    let final_tool_result = apply_tool_result_handler(tool_result, on_tool_result).await;
    if let Some(exec_result) = exec_result {
        let paired = match final_tool_result {
            Some(result) => Some(result),
            None => {
                let (text, is_error) = exec_result.describe_exec_result();
                pair(text, is_error).await
            }
        };
        return (exec_result, paired);
    }
    if let Some(final_tool_result) = final_tool_result {
        let built = build_from_tool_result(&final_tool_result);
        return (built, Some(final_tool_result));
    }
    let reason = "Tool returned no result";
    let tool_result = pair(reason.to_owned(), true).await;
    (build_rejected(reason), tool_result)
}

fn split_exec_handler_result<TResult>(result: CursorExecHandlerResult<TResult>) -> (Option<TResult>, Option<ToolResultMessage>) {
    match result {
        CursorExecHandlerResult::WithToolResult { result, tool_result } => (Some(result), tool_result),
        CursorExecHandlerResult::Result(result) => (Some(result), None),
        CursorExecHandlerResult::ToolResult(tool_result) => (None, Some(tool_result)),
    }
}

async fn apply_tool_result_handler(
    tool_result: Option<ToolResultMessage>,
    on_tool_result: Option<&CursorToolResultHandler>,
) -> Option<ToolResultMessage> {
    let tool_result = tool_result?;
    let Some(on_tool_result) = on_tool_result else { return Some(tool_result) };
    let updated = on_tool_result(tool_result.clone()).await;
    Some(updated.unwrap_or(tool_result))
}

/// Derive the transcript state of an exec result the handler returned in the
/// TResult-only form, which carries no `toolResult` to copy it from.
pub trait DescribeExecResult {
    fn describe_exec_result(&self) -> (String, bool);
}

/// Every exec result oneof is success-plus-failures; a variant this macro does
/// not name individually is a failure.
fn describe_other_variant<T>(_other: &T) -> (String, bool) {
    (String::new(), true)
}

fn describe_success_without_error() -> (String, bool) {
    ("Tool produced no transcript result".to_owned(), false)
}

fn describe_failure(error: Option<&str>, reason: Option<&str>, variant: &str) -> (String, bool) {
    match error.or(reason) {
        Some(text) if !text.is_empty() => (text.to_owned(), true),
        _ => (format!("Tool call {variant}"), true),
    }
}

macro_rules! describe_oneof_result {
    ($type:ty, $result_mod:path, $variant:ident, $success_ty:ty) => {
        impl DescribeExecResult for $type {
            fn describe_exec_result(&self) -> (String, bool) {
                use $result_mod as result_mod;
                match &self.result {
                    Some(result_mod::Result::Success(_)) => describe_success_without_error(),
                    Some(result_mod::Result::Error(error)) => {
                        describe_failure(Some(error.error.as_str()), None, stringify!($variant))
                    }
                    Some(result_mod::Result::Rejected(rejected)) => {
                        describe_failure(None, Some(rejected.reason.as_str()), stringify!($variant))
                    }
                    Some(other) => describe_other_variant(other),
                    None => describe_success_without_error(),
                }
            }
        }
    };
}

impl DescribeExecResult for McpResult {
    fn describe_exec_result(&self) -> (String, bool) {
        match &self.result {
            Some(mcp_result::Result::Success(success)) => {
                if !success.is_error {
                    return describe_success_without_error();
                }
                let text = mcp_content_to_text(&success.content);
                (
                    if text.is_empty() { "MCP tool reported an error".to_owned() } else { text },
                    true,
                )
            }
            Some(mcp_result::Result::Error(error)) => (error.error.clone(), true),
            Some(mcp_result::Result::Rejected(rejected)) => (rejected.reason.clone(), true),
            Some(mcp_result::Result::PermissionDenied(denied)) => (denied.error.clone(), true),
            Some(mcp_result::Result::ToolNotFound(_)) => {
                ("Tool call toolNotFound".to_owned(), true)
            }
            _ => describe_success_without_error(),
        }
    }
}

/// Flatten `McpSuccess.content` into transcript text.
fn mcp_content_to_text(content: &[McpToolResultContentItem]) -> String {
    let mut parts = Vec::new();
    for item in content {
        if let Some(mcp_tool_result_content_item::Content::Text(text)) = &item.content
            && !text.text.is_empty()
        {
            parts.push(text.text.clone());
        }
    }
    parts.join("\n")
}

macro_rules! describe_two_variant_result {
    ($type:ty, $result_mod:path, $variant:ident) => {
        impl DescribeExecResult for $type {
            fn describe_exec_result(&self) -> (String, bool) {
                use $result_mod as result_mod;
                match &self.result {
                    Some(result_mod::Result::Success(_)) => describe_success_without_error(),
                    Some(result_mod::Result::Error(error)) => {
                        describe_failure(Some(error.error.as_str()), None, stringify!($variant))
                    }
                    None => describe_success_without_error(),
                }
            }
        }
    };
}

macro_rules! describe_three_variant_result {
    ($type:ty, $result_mod:path, $variant:ident) => {
        impl DescribeExecResult for $type {
            fn describe_exec_result(&self) -> (String, bool) {
                use $result_mod as result_mod;
                match &self.result {
                    Some(result_mod::Result::Success(_)) => describe_success_without_error(),
                    Some(result_mod::Result::Error(error)) => {
                        describe_failure(Some(error.error.as_str()), None, stringify!($variant))
                    }
                    Some(result_mod::Result::Rejected(rejected)) => {
                        describe_failure(None, Some(rejected.reason.as_str()), stringify!($variant))
                    }
                    None => describe_success_without_error(),
                }
            }
        }
    };
}

describe_oneof_result!(ReadResult, super::cursor_agent::r#gen::agent_pb::read_result, read, ReadSuccess);
describe_oneof_result!(LsResult, super::cursor_agent::r#gen::agent_pb::ls_result, ls, LsSuccess);
describe_oneof_result!(WriteResult, super::cursor_agent::r#gen::agent_pb::write_result, write, WriteSuccess);
describe_oneof_result!(DeleteResult, super::cursor_agent::r#gen::agent_pb::delete_result, delete, DeleteSuccess);
describe_oneof_result!(DiagnosticsResult, super::cursor_agent::r#gen::agent_pb::diagnostics_result, diagnostics, DiagnosticsSuccess);
describe_two_variant_result!(GrepResult, super::cursor_agent::r#gen::agent_pb::grep_result, grep);
describe_three_variant_result!(PiEditExecResult, super::cursor_agent::r#gen::agent_pb::pi_edit_exec_result, piEdit);
describe_three_variant_result!(PiWriteExecResult, super::cursor_agent::r#gen::agent_pb::pi_write_exec_result, piWrite);
describe_two_variant_result!(PiReadExecResult, super::cursor_agent::r#gen::agent_pb::pi_read_exec_result, piRead);
describe_two_variant_result!(PiBashExecResult, super::cursor_agent::r#gen::agent_pb::pi_bash_exec_result, piBash);
describe_two_variant_result!(PiGrepExecResult, super::cursor_agent::r#gen::agent_pb::pi_grep_exec_result, piGrep);
describe_two_variant_result!(PiFindExecResult, super::cursor_agent::r#gen::agent_pb::pi_find_exec_result, piFind);
describe_two_variant_result!(PiLsExecResult, super::cursor_agent::r#gen::agent_pb::pi_ls_exec_result, piLs);

impl DescribeExecResult for ShellResult {
    fn describe_exec_result(&self) -> (String, bool) {
        match &self.result {
            Some(shell_result::Result::Success(_)) => describe_success_without_error(),
            Some(shell_result::Result::Failure(failure)) => (
                if failure.stderr.is_empty() { "Tool call failure".to_owned() } else { failure.stderr.clone() },
                true,
            ),
            Some(shell_result::Result::Rejected(rejected)) => (rejected.reason.clone(), true),
            Some(shell_result::Result::Timeout(_)) => ("Tool call timeout".to_owned(), true),
            Some(shell_result::Result::PermissionDenied(denied)) => (denied.error.clone(), true),
            Some(shell_result::Result::SpawnError(error)) => (error.error.clone(), true),
            None => describe_success_without_error(),
        }
    }
}

fn sanitize_shell_exec_result(exec_result: ShellResult) -> ShellResult {
    let Some(result) = exec_result.result.as_ref() else { return exec_result };
    match result {
        shell_result::Result::Success(value) => {
            let mut value = value.clone();
            if !value.stdout.is_empty() {
                value.stdout = sanitize_surrogates(&value.stdout);
            }
            if !value.stderr.is_empty() {
                value.stderr = sanitize_surrogates(&value.stderr);
            }
            ShellResult { result: Some(shell_result::Result::Success(value)), ..ShellResult::default() }
        }
        shell_result::Result::Failure(value) => {
            let mut value = value.clone();
            if !value.stdout.is_empty() {
                value.stdout = sanitize_surrogates(&value.stdout);
            }
            if !value.stderr.is_empty() {
                value.stderr = sanitize_surrogates(&value.stderr);
            }
            ShellResult { result: Some(shell_result::Result::Failure(value)), ..ShellResult::default() }
        }
        _ => exec_result,
    }
}

async fn send_shell_stream_event(transport: &CursorTransport, exec_msg: &ExecServerMessage, event: shell_stream::Event) {
    send_exec_client_message(transport, exec_msg, exec_client_message::Message::ShellStream(ShellStream { event: Some(event) })).await;
}

/// Length of a trailing incomplete ANSI escape sequence: the TS
/// /\x1b(|\u{5b}|\u{5b}\d*|\u{5b}\?|\u{5b}\?\d*|\u{5d}\d*;?)$/ match, or 0.
fn incomplete_escape_suffix(buffer: &str) -> usize {
    let chars: Vec<char> = buffer.chars().collect();
    let len = chars.len();
    for start in (0..len).rev() {
        if chars[start] != '\u{1b}' {
            continue;
        }
        let rest: Vec<char> = chars[start..].to_vec();
        let digits = |from: usize| rest[from..].iter().all(char::is_ascii_digit);
        let is_incomplete = match rest.len() {
            1 => true,
            2 => rest[1] == '[',
            3 => rest[1] == '[' && (digits(2) || rest[2] == '?'),
            _ if rest[1] == '[' && rest[2] == '?' => digits(3),
            _ if rest[1] == '[' => digits(2),
            _ if rest[1] == ']' => rest[2..].iter().all(|c| c.is_ascii_digit() || *c == ';'),
            _ => false,
        };
        if is_incomplete {
            return len - start;
        }
        break;
    }
    0
}

async fn handle_shell_stream_args(
    args: &ShellArgs,
    exec_msg: &ExecServerMessage,
    transport: &CursorTransport,
    exec_handlers: Option<&CursorExecHandlers>,
    on_tool_result: Option<&CursorToolResultHandler>,
) {
    let working_directory = if args.working_directory.is_empty() {
        std::env::current_dir().map(|path| path.display().to_string()).unwrap_or_default()
    } else {
        args.working_directory.clone()
    };
    let normalized_args = ShellArgs { working_directory: working_directory.clone(), ..args.clone() };

    send_shell_stream_event(transport, exec_msg, shell_stream::Event::Start(Default::default())).await;

    let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel::<shell_stream::Event>();
    let event_writer = {
        let transport = transport.clone();
        let exec_msg = exec_msg.clone();
        tokio::spawn(async move {
            while let Some(event) = event_rx.recv().await {
                send_shell_stream_event(&transport, &exec_msg, event).await;
            }
        })
    };
    let stream_buffer = Arc::new(Mutex::new(ShellStreamBuffer::new(event_tx.clone())));
    let handler = exec_handlers.and_then(|handlers| {
        if let Some(shell_stream) = handlers.shell_stream.clone() {
            let stream_buffer = stream_buffer.clone();
            Some(Arc::new(move |args: ShellArgs| {
                shell_stream(args, Box::new(ShellStreamCallbackHandle { buffer: stream_buffer.clone() }))
            }) as Arc<dyn Fn(ShellArgs) -> crate::api::cursor_agent::types::ExecFuture<ShellResult> + Send + Sync>)
        } else {
            handlers.shell.clone()
        }
    });

    let (exec_result, _tool_result) = resolve_exec_handler(
        normalized_args.clone(),
        handler.as_ref(),
        on_tool_result,
        |tool_result| build_shell_result_from_tool_result(&normalized_args, tool_result),
        |reason| build_shell_rejected_result(&normalized_args.command, &normalized_args.working_directory, reason),
        |error| build_shell_failure_result(&normalized_args.command, &normalized_args.working_directory, error),
        Some(&CursorExecPairing { tool_call_id: args.tool_call_id.clone(), tool_name: "bash".to_owned() }),
    )
    .await;

    if exec_handlers.and_then(|handlers| handlers.shell_stream.as_ref()).is_some() {
        let mut buffer = stream_buffer.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        buffer.flush_stdout();
        buffer.flush_stderr();
    }
    drop(event_tx);
    let _ = event_writer.await;

    let send_buffered_output = exec_handlers.and_then(|handlers| handlers.shell_stream.as_ref()).is_none();
    let sanitized = sanitize_shell_exec_result(exec_result);
    send_shell_stream_exit_from_result(transport, exec_msg, &sanitized, send_buffered_output).await;
    send_exec_client_message(transport, exec_msg, exec_client_message::Message::ShellResult(sanitized)).await;
}

/// Forwards a streaming shell's output chunks in real time, holding back an
/// incomplete trailing ANSI escape sequence so a split sequence is never sent
/// as a separate frame.
struct ShellStreamBuffer {
    sender: tokio::sync::mpsc::UnboundedSender<shell_stream::Event>,
    stdout: String,
    stderr: String,
}

impl ShellStreamBuffer {
    fn new(sender: tokio::sync::mpsc::UnboundedSender<shell_stream::Event>) -> Self {
        Self { sender, stdout: String::new(), stderr: String::new() }
    }

    fn push(&mut self, stdout: bool, data: &str) {
        let buffer = if stdout { &mut self.stdout } else { &mut self.stderr };
        buffer.push_str(data);
        if buffer.contains('\n') || buffer.len() > 4096 {
            self.flush(stdout);
        }
    }

    fn flush(&mut self, stdout: bool) {
        let buffer = if stdout { &mut self.stdout } else { &mut self.stderr };
        if buffer.is_empty() {
            return;
        }
        let safe_end = buffer.len() - incomplete_escape_suffix(buffer);
        let to_send = buffer[..safe_end].to_owned();
        let remaining = buffer[safe_end..].to_owned();
        *buffer = remaining;
        if to_send.is_empty() {
            return;
        }
        let data = sanitize_surrogates(&to_send);
        let event = if stdout {
            shell_stream::Event::Stdout(ShellStreamStdout { data })
        } else {
            shell_stream::Event::Stderr(ShellStreamStderr { data })
        };
        let _ = self.sender.send(event);
    }

    fn flush_stdout(&mut self) {
        self.flush(true);
    }

    fn flush_stderr(&mut self) {
        self.flush(false);
    }
}

struct ShellStreamCallbackHandle {
    buffer: Arc<Mutex<ShellStreamBuffer>>,
}

impl CursorShellStreamCallbacks for ShellStreamCallbackHandle {
    fn on_stdout(&mut self, data: &str) {
        self.buffer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(true, data);
    }

    fn on_stderr(&mut self, data: &str) {
        self.buffer.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(false, data);
    }
}

async fn send_shell_stream_exit_from_result(
    transport: &CursorTransport,
    exec_msg: &ExecServerMessage,
    exec_result: &ShellResult,
    send_buffered_output: bool,
) {
    let Some(result) = exec_result.result.as_ref() else { return };
    match result {
        shell_result::Result::Success(value) => {
            if send_buffered_output {
                if !value.stdout.is_empty() {
                    send_shell_stream_event(transport, exec_msg, shell_stream::Event::Stdout(ShellStreamStdout { data: sanitize_surrogates(&value.stdout) })).await;
                }
                if !value.stderr.is_empty() {
                    send_shell_stream_event(transport, exec_msg, shell_stream::Event::Stderr(ShellStreamStderr { data: sanitize_surrogates(&value.stderr) })).await;
                }
            }
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::Exit(ShellStreamExit {
                code: value.exit_code as u32,
                cwd: value.working_directory.clone(),
                aborted: false,
                ..ShellStreamExit::default()
            })).await;
        }
        shell_result::Result::Failure(value) => {
            if send_buffered_output {
                if !value.stdout.is_empty() {
                    send_shell_stream_event(transport, exec_msg, shell_stream::Event::Stdout(ShellStreamStdout { data: sanitize_surrogates(&value.stdout) })).await;
                }
                if !value.stderr.is_empty() {
                    send_shell_stream_event(transport, exec_msg, shell_stream::Event::Stderr(ShellStreamStderr { data: sanitize_surrogates(&value.stderr) })).await;
                }
            }
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::Exit(ShellStreamExit {
                code: value.exit_code as u32,
                cwd: value.working_directory.clone(),
                aborted: value.aborted,
                abort_reason: value.abort_reason,
                ..ShellStreamExit::default()
            })).await;
        }
        shell_result::Result::Rejected(value) => {
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::Rejected(value.clone())).await;
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::Exit(ShellStreamExit {
                code: 1,
                cwd: value.working_directory.clone(),
                aborted: false,
                ..ShellStreamExit::default()
            })).await;
        }
        shell_result::Result::Timeout(value) => {
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::Stderr(ShellStreamStderr {
                data: format!("Command timed out after {}ms", value.timeout_ms),
            })).await;
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::Exit(ShellStreamExit {
                code: 1,
                cwd: value.working_directory.clone(),
                aborted: true,
                ..ShellStreamExit::default()
            })).await;
        }
        shell_result::Result::PermissionDenied(value) => {
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::PermissionDenied(value.clone())).await;
            send_shell_stream_event(transport, exec_msg, shell_stream::Event::Exit(ShellStreamExit {
                code: 1,
                cwd: value.working_directory.clone(),
                aborted: false,
                ..ShellStreamExit::default()
            })).await;
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_exec_server_message(
    exec_msg: ExecServerMessage,
    transport: &CursorTransport,
    exec_handlers: Option<&CursorExecHandlers>,
    on_tool_result: Option<&CursorToolResultHandler>,
    request_context_tools: &[McpToolDefinition],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    state: &mut BlockState,
) {
    let Some(exec_case) = exec_msg.message.as_ref() else {
        log("warn", Some("unknownExecVariant"), Some(json!({ "id": exec_msg.id, "execId": exec_msg.exec_id })));
        send_exec_client_throw(transport, &exec_msg, "Unknown exec message variant", Some("unknown_exec_variant")).await;
        send_exec_client_stream_close(transport, &exec_msg).await;
        return;
    };
    log(
        "exec",
        Some("dispatch"),
        Some(json!({ "execCase": exec_server_case_name(exec_case), "execId": exec_msg.exec_id, "hasHandlers": exec_handlers.is_some() })),
    );
    let heartbeat = arm_cursor_exec_heartbeat(transport, &exec_msg);
    let dispatch = dispatch_exec_server_message(
        &exec_msg,
        transport,
        exec_handlers,
        on_tool_result,
        request_context_tools,
        output,
        stream,
        state,
    )
    .await;
    if let Err(error) = dispatch {
        log("error", Some("execDispatch"), Some(json!({ "error": error, "id": exec_msg.id, "execId": exec_msg.exec_id })));
        send_exec_client_throw(transport, &exec_msg, "Local exec dispatch failed", Some("exec_dispatch_failed")).await;
    }
    heartbeat.abort();
    send_exec_client_stream_close(transport, &exec_msg).await;
}

fn exec_server_case_name(message: &exec_server_message::Message) -> &'static str {
    match message {
        exec_server_message::Message::ReadArgs(_) => "readArgs",
        exec_server_message::Message::LsArgs(_) => "lsArgs",
        exec_server_message::Message::GrepArgs(_) => "grepArgs",
        exec_server_message::Message::WriteArgs(_) => "writeArgs",
        exec_server_message::Message::DeleteArgs(_) => "deleteArgs",
        exec_server_message::Message::ShellArgs(_) => "shellArgs",
        exec_server_message::Message::ShellStreamArgs(_) => "shellStreamArgs",
        exec_server_message::Message::DiagnosticsArgs(_) => "diagnosticsArgs",
        exec_server_message::Message::McpArgs(_) => "mcpArgs",
        exec_server_message::Message::RequestContextArgs(_) => "requestContextArgs",
        exec_server_message::Message::PiReadArgs(_) => "piReadArgs",
        exec_server_message::Message::PiBashArgs(_) => "piBashArgs",
        exec_server_message::Message::PiEditArgs(_) => "piEditArgs",
        exec_server_message::Message::PiWriteArgs(_) => "piWriteArgs",
        exec_server_message::Message::PiGrepArgs(_) => "piGrepArgs",
        exec_server_message::Message::PiFindArgs(_) => "piFindArgs",
        exec_server_message::Message::PiLsArgs(_) => "piLsArgs",
        exec_server_message::Message::McpStateExecArgs(_) => "mcpStateExecArgs",
        exec_server_message::Message::ExecuteHookArgs(_) => "executeHookArgs",
        exec_server_message::Message::GitDiffRequest(_) => "gitDiffRequest",
        _ => "otherExecMessage",
    }
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_exec_server_message(
    exec_msg: &ExecServerMessage,
    transport: &CursorTransport,
    exec_handlers: Option<&CursorExecHandlers>,
    on_tool_result: Option<&CursorToolResultHandler>,
    request_context_tools: &[McpToolDefinition],
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    state: &mut BlockState,
) -> Result<(), String> {
    let Some(exec_case) = exec_msg.message.as_ref() else {
        return Err("Expected a recognized exec message".to_owned());
    };

    if let exec_server_message::Message::RequestContextArgs(_) = exec_case {
        let request_context = RequestContext {
            rules: Vec::new(),
            repository_info: Vec::new(),
            tools: request_context_tools.to_vec(),
            git_repos: Vec::new(),
            project_layouts: Vec::new(),
            mcp_instructions: Vec::new(),
            file_contents: HashMap::new(),
            custom_subagents: Vec::new(),
            ..RequestContext::default()
        };
        let request_context_result = RequestContextResult {
            result: Some(super::cursor_agent::r#gen::agent_pb::request_context_result::Result::Success(
                RequestContextSuccess { request_context: Some(request_context), ..RequestContextSuccess::default() },
            )),
        };
        send_exec_client_message(transport, exec_msg, exec_client_message::Message::RequestContextResult(request_context_result)).await;
        return Ok(());
    }

    match exec_case {
        exec_server_message::Message::ReadArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            let mut block_args = Map::new();
            block_args.insert("path".to_owned(), json!(args.path));
            block_args.insert("offset".to_owned(), args.offset.map(Value::from).unwrap_or(Value::Null));
            block_args.insert("limit".to_owned(), args.limit.map(Value::from).unwrap_or(Value::Null));
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "read", block_args);
            let range_applied = args.offset.is_some() || args.limit.is_some();
            let (exec_result, _) = resolve_exec_handler(
                args.clone(),
                exec_handlers.and_then(|handlers| handlers.read.as_ref()),
                on_tool_result,
                |tool_result| build_read_result_from_tool_result(&args.path, tool_result, range_applied),
                |reason| build_read_rejected_result(&args.path, reason),
                |error| build_read_error_result(&args.path, error),
                Some(&CursorExecPairing { tool_call_id, tool_name: "read".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ReadResult(exec_result)).await;
        }
        exec_server_message::Message::LsArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            let block_args = Map::from_iter([("path".to_owned(), json!(pi_ls_path(Some(&args.path))))]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "ls", block_args);
            let (exec_result, _) = resolve_exec_handler(
                args.clone(),
                exec_handlers.and_then(|handlers| handlers.ls.as_ref()),
                on_tool_result,
                |tool_result| build_ls_result_from_tool_result(&args.path, tool_result),
                |reason| build_ls_rejected_result(&args.path, reason),
                |error| build_ls_error_result(&args.path, error),
                Some(&CursorExecPairing { tool_call_id, tool_name: "ls".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::LsResult(exec_result)).await;
        }
        exec_server_message::Message::GrepArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            if let Some(error) = empty_grep_pattern_rejection(Some(&args.pattern), args.glob.as_deref()) {
                send_exec_client_message(transport, exec_msg, exec_client_message::Message::GrepResult(build_grep_error_result(&error))).await;
                return Ok(());
            }
            let mut block_args = Map::new();
            block_args.insert("pattern".to_owned(), json!(args.pattern));
            block_args.insert(
                "path".to_owned(),
                args.path.as_deref().filter(|path| !path.is_empty()).map(Value::from).unwrap_or(Value::Null),
            );
            block_args.insert(
                "glob".to_owned(),
                args.glob.as_deref().filter(|glob| !glob.is_empty()).map(Value::from).unwrap_or(Value::Null),
            );
            block_args.insert(
                "ignoreCase".to_owned(),
                if args.case_insensitive == Some(true) { Value::Bool(true) } else { Value::Null },
            );
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "grep", block_args);
            let (exec_result, _) = resolve_exec_handler(
                args.clone(),
                exec_handlers.and_then(|handlers| handlers.grep.as_ref()),
                on_tool_result,
                |tool_result| build_grep_result_from_tool_result(args, tool_result),
                build_grep_error_result,
                build_grep_error_result,
                Some(&CursorExecPairing { tool_call_id, tool_name: "grep".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::GrepResult(exec_result)).await;
        }
        exec_server_message::Message::WriteArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            let content = if args.file_text.is_empty() {
                String::from_utf8_lossy(&args.file_bytes).into_owned()
            } else {
                args.file_text.clone()
            };
            let block_args = Map::from_iter([("path".to_owned(), json!(args.path)), ("content".to_owned(), json!(content))]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "write", block_args);
            let write_args = WriteResultArgs {
                path: &args.path,
                file_text: if args.file_text.is_empty() { None } else { Some(args.file_text.as_str()) },
                file_bytes: if args.file_bytes.is_empty() { None } else { Some(&args.file_bytes) },
                return_file_content_after_write: Some(args.return_file_content_after_write),
            };
            let (exec_result, _) = resolve_exec_handler(
                args.clone(),
                exec_handlers.and_then(|handlers| handlers.write.as_ref()),
                on_tool_result,
                |tool_result| build_write_result_from_tool_result(&write_args, tool_result),
                |reason| build_write_rejected_result(&args.path, reason),
                |error| build_write_error_result(&args.path, error),
                Some(&CursorExecPairing { tool_call_id, tool_name: "write".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::WriteResult(exec_result)).await;
        }
        exec_server_message::Message::DeleteArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            let block_args = Map::from_iter([("path".to_owned(), json!(args.path))]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "delete", block_args);
            let (exec_result, _) = resolve_exec_handler(
                args.clone(),
                exec_handlers.and_then(|handlers| handlers.delete.as_ref()),
                on_tool_result,
                |tool_result| build_delete_result_from_tool_result(&args.path, tool_result),
                |reason| build_delete_rejected_result(&args.path, reason),
                |error| build_delete_error_result(&args.path, error),
                Some(&CursorExecPairing { tool_call_id, tool_name: "delete".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::DeleteResult(exec_result)).await;
        }
        exec_server_message::Message::ShellArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            let normalized = ShellArgs {
                working_directory: if args.working_directory.is_empty() {
                    std::env::current_dir().map(|path| path.display().to_string()).unwrap_or_default()
                } else {
                    args.working_directory.clone()
                },
                ..args.clone()
            };
            let shell_timeout = if args.timeout > 0 { Some(args.timeout) } else { None };
            let block_args = Map::from_iter([
                (
                    "command".to_owned(),
                    json!(compose_shell_command(
                        &args.command,
                        if args.working_directory.is_empty() { None } else { Some(&args.working_directory) }
                    )),
                ),
                ("timeout".to_owned(), shell_timeout.map(Value::from).unwrap_or(Value::Null)),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "bash", block_args);
            let (exec_result, _) = resolve_exec_handler(
                normalized.clone(),
                exec_handlers.and_then(|handlers| handlers.shell.as_ref()),
                on_tool_result,
                |tool_result| build_shell_result_from_tool_result(&normalized, tool_result),
                |reason| build_shell_rejected_result(&normalized.command, &normalized.working_directory, reason),
                |error| build_shell_failure_result(&normalized.command, &normalized.working_directory, error),
                Some(&CursorExecPairing { tool_call_id, tool_name: "bash".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ShellResult(sanitize_shell_exec_result(exec_result))).await;
        }
        exec_server_message::Message::ShellStreamArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            let shell_timeout = if args.timeout > 0 { Some(args.timeout) } else { None };
            let block_args = Map::from_iter([
                (
                    "command".to_owned(),
                    json!(compose_shell_command(
                        &args.command,
                        if args.working_directory.is_empty() { None } else { Some(&args.working_directory) }
                    )),
                ),
                ("timeout".to_owned(), shell_timeout.map(Value::from).unwrap_or(Value::Null)),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id, "bash", block_args);
            handle_shell_stream_args(args, exec_msg, transport, exec_handlers, on_tool_result).await;
        }
        exec_server_message::Message::MiniSweAgentBashArgs(args) => {
            let mut tool_call_id = Some(args.tool_call_id.clone());
            ensure_unique_cursor_exec_tool_call_id(output, &mut tool_call_id);
            let tool_call_id = tool_call_id.unwrap_or_default();
            let normalized = ShellArgs {
                working_directory: if args.working_directory.is_empty() {
                    std::env::current_dir().map(|path| path.display().to_string()).unwrap_or_default()
                } else {
                    args.working_directory.clone()
                },
                ..args.clone()
            };
            let block_args = Map::from_iter([
                (
                    "command".to_owned(),
                    json!(compose_shell_command(
                        &args.command,
                        if args.working_directory.is_empty() { None } else { Some(&args.working_directory) }
                    )),
                ),
                (
                    "timeout".to_owned(),
                    if args.timeout > 0 { json!(args.timeout) } else { Value::Null },
                ),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "bash", block_args);
            let (exec_result, _) = resolve_exec_handler(
                normalized.clone(),
                exec_handlers.and_then(|handlers| handlers.shell.as_ref()),
                on_tool_result,
                |tool_result| build_shell_result_from_tool_result(&normalized, tool_result),
                |reason| build_shell_rejected_result(&normalized.command, &normalized.working_directory, reason),
                |error| build_shell_failure_result(&normalized.command, &normalized.working_directory, error),
                Some(&CursorExecPairing { tool_call_id, tool_name: "bash".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::MiniSweAgentBashResult(sanitize_shell_exec_result(exec_result))).await;
        }
        exec_server_message::Message::DiagnosticsArgs(args) => {
            let tool_call_id = Some(args.tool_call_id.clone()).filter(|id| !id.is_empty()).unwrap_or_else(random_uuid);
            let _ = &tool_call_id;
            let (exec_result, _) = resolve_exec_handler(
                args.clone(),
                exec_handlers.and_then(|handlers| handlers.diagnostics.as_ref()),
                on_tool_result,
                |tool_result| build_diagnostics_result_from_tool_result(&args.path, tool_result),
                |reason| build_diagnostics_rejected_result(&args.path, reason),
                build_diagnostics_error_result,
                None,
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::DiagnosticsResult(exec_result)).await;
        }
        exec_server_message::Message::McpArgs(args) => {
            let mcp_call = decode_mcp_call(args);
            if mcp_call.approval_only == Some(true) {
                let approved = match exec_handlers.and_then(|handlers| handlers.mcp_approval_preflight.clone()) {
                    Some(preflight) => preflight(mcp_call.clone()).await,
                    None => false,
                };
                let result = McpResult {
                    result: Some(if approved {
                        mcp_result::Result::Approved(McpApproved::default())
                    } else {
                        mcp_result::Result::Rejected(McpRejected {
                            reason: format!(
                                "Tool \"{}\" is not approved to run without asking.",
                                if mcp_call.tool_name.is_empty() { mcp_call.name.clone() } else { mcp_call.tool_name.clone() }
                            ),
                            is_readonly: false,
                        })
                    }),
                };
                send_exec_client_message(transport, exec_msg, exec_client_message::Message::McpResult(result)).await;
                return Ok(());
            }
            let has_mcp_handler = exec_handlers.and_then(|handlers| handlers.mcp.as_ref()).is_some();
            if has_mcp_handler {
                let existing = output.content.iter().position(|block| {
                    matches!(block, ContentBlock::ToolCall(call) if call.id == mcp_call.tool_call_id)
                });
                match existing {
                    Some(index) => mark_cursor_exec_resolved(state, index),
                    None => {
                        let name = if mcp_call.tool_name.is_empty() { mcp_call.name.clone() } else { mcp_call.tool_name.clone() };
                        synthesize_cursor_exec_tool_call(output, stream, state, mcp_call.tool_call_id.clone(), &name, mcp_call.args.clone());
                        state.resolved_mcp_tool_call_ids.insert(mcp_call.tool_call_id.clone());
                    }
                }
            }
            let pairing = has_mcp_handler.then(|| CursorExecPairing {
                tool_call_id: mcp_call.tool_call_id.clone(),
                tool_name: mcp_call.tool_name.clone(),
            });
            let (exec_result, _) = resolve_exec_handler(
                mcp_call.clone(),
                exec_handlers.and_then(|handlers| handlers.mcp.as_ref()),
                on_tool_result,
                build_mcp_result_from_tool_result,
                |_reason| build_mcp_tool_not_found_result(&mcp_call),
                build_mcp_error_result,
                pairing.as_ref(),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::McpResult(exec_result)).await;
        }
        exec_server_message::Message::PiReadArgs(args) => {
            let tool_call_id = random_uuid();
            let read_args = pi_read_args(
                &args.path,
                args.offset.map(f64::from),
                args.limit.map(f64::from),
            );
            let block_args = match read_args {
                Some(read_args) => {
                    let mut block_args = Map::new();
                    block_args.insert("path".to_owned(), json!(read_args.path));
                    block_args.insert(
                        "offset".to_owned(),
                        read_args.offset.map(Value::from).unwrap_or(Value::Null),
                    );
                    block_args.insert(
                        "limit".to_owned(),
                        read_args.limit.map(Value::from).unwrap_or(Value::Null),
                    );
                    block_args
                }
                None => Map::from_iter([
                    ("path".to_owned(), json!(args.path)),
                    ("offset".to_owned(), args.offset.map(Value::from).unwrap_or(Value::Null)),
                    ("limit".to_owned(), json!(0)),
                ]),
            };
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "read", block_args);
            let call = CursorPiCall { args: args.clone(), tool_call_id: tool_call_id.clone() };
            let (exec_result, _) = resolve_exec_handler(
                call,
                exec_handlers.and_then(|handlers| handlers.pi_read.as_ref()),
                on_tool_result,
                build_pi_read_result,
                build_pi_read_error,
                build_pi_read_error,
                Some(&CursorExecPairing { tool_call_id, tool_name: "read".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::PiReadResult(exec_result)).await;
        }
        exec_server_message::Message::PiBashArgs(args) => {
            let tool_call_id = random_uuid();
            let block_args = Map::from_iter([
                ("command".to_owned(), json!(args.command)),
                ("timeout".to_owned(), pi_timeout(args.timeout).map(Value::from).unwrap_or(Value::Null)),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "bash", block_args);
            let call = CursorPiCall { args: args.clone(), tool_call_id: tool_call_id.clone() };
            let (exec_result, _) = resolve_exec_handler(
                call,
                exec_handlers.and_then(|handlers| handlers.pi_bash.as_ref()),
                on_tool_result,
                build_pi_bash_result,
                build_pi_bash_error,
                build_pi_bash_error,
                Some(&CursorExecPairing { tool_call_id, tool_name: "bash".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::PiBashResult(exec_result)).await;
        }
        exec_server_message::Message::PiEditArgs(args) => {
            let tool_call_id = random_uuid();
            let edits: Vec<Value> = args
                .edits
                .iter()
                .map(|edit| json!({ "oldText": edit.old_text, "newText": edit.new_text }))
                .collect();
            let block_args = Map::from_iter([("path".to_owned(), json!(args.path)), ("edits".to_owned(), Value::Array(edits))]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "edit", block_args);
            let call = CursorPiCall { args: args.clone(), tool_call_id: tool_call_id.clone() };
            let (exec_result, _) = resolve_exec_handler(
                call,
                exec_handlers.and_then(|handlers| handlers.pi_edit.as_ref()),
                on_tool_result,
                build_pi_edit_result,
                build_pi_edit_rejected,
                build_pi_edit_error,
                Some(&CursorExecPairing { tool_call_id, tool_name: "edit".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::PiEditResult(exec_result)).await;
        }
        exec_server_message::Message::PiWriteArgs(args) => {
            let tool_call_id = random_uuid();
            let block_args = Map::from_iter([("path".to_owned(), json!(args.path)), ("content".to_owned(), json!(args.content))]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "write", block_args);
            let call = CursorPiCall { args: args.clone(), tool_call_id: tool_call_id.clone() };
            let (exec_result, _) = resolve_exec_handler(
                call,
                exec_handlers.and_then(|handlers| handlers.pi_write.as_ref()),
                on_tool_result,
                build_pi_write_result,
                build_pi_write_rejected,
                build_pi_write_error,
                Some(&CursorExecPairing { tool_call_id, tool_name: "write".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::PiWriteResult(exec_result)).await;
        }
        exec_server_message::Message::PiGrepArgs(args) => {
            let tool_call_id = random_uuid();
            let block_args = Map::from_iter([
                ("pattern".to_owned(), json!(args.pattern)),
                ("path".to_owned(), if args.path.as_deref().unwrap_or("").is_empty() { Value::Null } else { json!(args.path) }),
                ("glob".to_owned(), if args.glob.as_deref().unwrap_or("").is_empty() { Value::Null } else { json!(args.glob) }),
                ("ignoreCase".to_owned(), if args.ignore_case == Some(true) { Value::Bool(true) } else { Value::Null }),
                ("literal".to_owned(), if args.literal == Some(true) { Value::Bool(true) } else { Value::Null }),
                ("context".to_owned(), args.context.map(Value::from).unwrap_or(Value::Null)),
                ("limit".to_owned(), pi_limit(args.limit.map(f64::from)).map(Value::from).unwrap_or(Value::Null)),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "grep", block_args);
            let call = CursorPiCall { args: args.clone(), tool_call_id: tool_call_id.clone() };
            let (exec_result, _) = resolve_exec_handler(
                call,
                exec_handlers.and_then(|handlers| handlers.pi_grep.as_ref()),
                on_tool_result,
                build_pi_grep_result,
                build_pi_grep_error,
                build_pi_grep_error,
                Some(&CursorExecPairing { tool_call_id, tool_name: "grep".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::PiGrepResult(exec_result)).await;
        }
        exec_server_message::Message::PiFindArgs(args) => {
            let tool_call_id = random_uuid();
            let block_args = Map::from_iter([
                ("pattern".to_owned(), json!(args.pattern)),
                ("path".to_owned(), if args.path.as_deref().unwrap_or("").is_empty() { Value::Null } else { json!(args.path) }),
                ("limit".to_owned(), pi_limit(args.limit.map(f64::from)).map(Value::from).unwrap_or(Value::Null)),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "find", block_args);
            let call = CursorPiCall { args: args.clone(), tool_call_id: tool_call_id.clone() };
            let (exec_result, _) = resolve_exec_handler(
                call,
                exec_handlers.and_then(|handlers| handlers.pi_find.as_ref()),
                on_tool_result,
                build_pi_find_result,
                build_pi_find_error,
                build_pi_find_error,
                Some(&CursorExecPairing { tool_call_id, tool_name: "find".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::PiFindResult(exec_result)).await;
        }
        exec_server_message::Message::PiLsArgs(args) => {
            let tool_call_id = random_uuid();
            let block_args = Map::from_iter([
                ("path".to_owned(), json!(pi_ls_path(args.path.as_deref()))),
                ("limit".to_owned(), pi_limit(args.limit.map(f64::from)).map(Value::from).unwrap_or(Value::Null)),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "ls", block_args);
            let call = CursorPiCall { args: args.clone(), tool_call_id: tool_call_id.clone() };
            let (exec_result, _) = resolve_exec_handler(
                call,
                exec_handlers.and_then(|handlers| handlers.pi_ls.as_ref()),
                on_tool_result,
                build_pi_ls_result,
                build_pi_ls_error,
                build_pi_ls_error,
                Some(&CursorExecPairing { tool_call_id, tool_name: "ls".to_owned() }),
            )
            .await;
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::PiLsResult(exec_result)).await;
        }
        exec_server_message::Message::ListMcpResourcesExecArgs(_) => {
            let exec_result = ListMcpResourcesExecResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::list_mcp_resources_exec_result::Result::Success(
                    ListMcpResourcesSuccess { resources: Vec::new() },
                )),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ListMcpResourcesExecResult(exec_result)).await;
        }
        exec_server_message::Message::ReadMcpResourceExecArgs(args) => {
            let exec_result = ReadMcpResourceExecResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::read_mcp_resource_exec_result::Result::NotFound(
                    ReadMcpResourceNotFound { uri: args.uri.clone() },
                )),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ReadMcpResourceExecResult(exec_result)).await;
        }
        exec_server_message::Message::RecordScreenArgs(_) => {
            let exec_result = RecordScreenResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::record_screen_result::Result::Failure(RecordScreenFailure {
                    error: NOT_IMPLEMENTED.to_owned(),
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::RecordScreenResult(exec_result)).await;
        }
        exec_server_message::Message::ComputerUseArgs(_) => {
            let exec_result = ComputerUseResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::computer_use_result::Result::Error(ComputerUseError {
                    error: NOT_IMPLEMENTED.to_owned(),
                    ..ComputerUseError::default()
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ComputerUseResult(exec_result)).await;
        }
        exec_server_message::Message::BackgroundShellSpawnArgs(args) => {
            let exec_result = BackgroundShellSpawnResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::background_shell_spawn_result::Result::Rejected(ShellRejected {
                    command: args.command.clone(),
                    working_directory: args.working_directory.clone(),
                    reason: "Not implemented".to_owned(),
                    is_readonly: false,
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::BackgroundShellSpawnResult(exec_result)).await;
        }
        exec_server_message::Message::WriteShellStdinArgs(_) => {
            let exec_result = WriteShellStdinResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::write_shell_stdin_result::Result::Error(WriteShellStdinError {
                    error: "Not implemented".to_owned(),
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::WriteShellStdinResult(exec_result)).await;
        }
        exec_server_message::Message::FetchArgs(args) => {
            let exec_result = FetchResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::fetch_result::Result::Error(FetchError {
                    url: args.url.clone(),
                    error: "Not implemented".to_owned(),
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::FetchResult(exec_result)).await;
        }
        exec_server_message::Message::RedactedReadArgs(args) => {
            send_exec_client_message(
                transport,
                exec_msg,
                exec_client_message::Message::RedactedReadResult(build_read_error_result(
                    &args.path,
                    "Secret redaction is not implemented by this client",
                )),
            )
            .await;
        }
        exec_server_message::Message::McpStateExecArgs(args) => {
            send_exec_client_message(
                transport,
                exec_msg,
                exec_client_message::Message::McpStateExecResult(build_mcp_state_result(request_context_tools, &args.server_identifiers)),
            )
            .await;
        }
        exec_server_message::Message::ExecuteHookArgs(args) => {
            match build_neutral_hook_result(args.request.as_ref()) {
                Some(exec_result) => {
                    send_exec_client_message(transport, exec_msg, exec_client_message::Message::ExecuteHookResult(exec_result)).await;
                }
                None => {
                    let case = args
                        .request
                        .as_ref()
                        .and_then(|request| request.request.as_ref())
                        .map(execute_hook_case_name)
                        .unwrap_or("unset");
                    send_exec_client_throw(transport, exec_msg, &format!("Unsupported hook request: {case}"), Some("unknown_hook_request")).await;
                }
            }
        }
        exec_server_message::Message::SubagentArgs(_) => {
            let exec_result = SubagentResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::subagent_result::Result::Error(SubagentError {
                    error: format!("Subagents are {NOT_IMPLEMENTED_SUFFIX}"),
                    ..SubagentError::default()
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::SubagentResult(exec_result)).await;
        }
        exec_server_message::Message::SubagentAwaitArgs(args) => {
            let exec_result = SubagentAwaitResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::subagent_await_result::Result::NotFound(SubagentAwaitNotFound {
                    agent_id: args.agent_id.clone(),
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::SubagentAwaitResult(exec_result)).await;
        }
        exec_server_message::Message::ForceBackgroundShellArgs(_) => {
            let exec_result = ForceBackgroundShellResult {
                status: ForceBackgroundShellStatus::NotFound as i32,
                ..ForceBackgroundShellResult::default()
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ForceBackgroundShellResult(exec_result)).await;
        }
        exec_server_message::Message::ForceBackgroundSubagentArgs(_) => {
            let exec_result = ForceBackgroundSubagentResult { status: ForceBackgroundSubagentStatus::NotFound as i32 };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ForceBackgroundSubagentResult(exec_result)).await;
        }
        exec_server_message::Message::SmartModeClassifierArgs(_) => {
            let exec_result = SmartModeClassifierResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::smart_mode_classifier_result::Result::Error(
                    SmartModeClassifierError { error: format!("Smart-mode classification is {NOT_IMPLEMENTED_SUFFIX}") },
                )),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::SmartModeClassifierResult(exec_result)).await;
        }
        exec_server_message::Message::CanvasDiagnosticsArgs(args) => {
            let exec_result = CanvasDiagnosticsResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::canvas_diagnostics_result::Result::Error(CanvasDiagnosticsError {
                    path: args.path.clone(),
                    error: format!("Canvas diagnostics are {NOT_IMPLEMENTED_SUFFIX}"),
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::CanvasDiagnosticsResult(exec_result)).await;
        }
        exec_server_message::Message::ShellAllowlistPrecheckArgs(_) => {
            send_exec_client_message(
                transport,
                exec_msg,
                exec_client_message::Message::ShellAllowlistPrecheckResult(ShellAllowlistPrecheckResult { allowlisted: false }),
            )
            .await;
        }
        exec_server_message::Message::McpAllowlistPrecheckArgs(_) => {
            send_exec_client_message(
                transport,
                exec_msg,
                exec_client_message::Message::McpAllowlistPrecheckResult(McpAllowlistPrecheckResult { allowlisted: false }),
            )
            .await;
        }
        exec_server_message::Message::WebFetchAllowlistPrecheckArgs(_) => {
            send_exec_client_message(
                transport,
                exec_msg,
                exec_client_message::Message::WebFetchAllowlistPrecheckResult(WebFetchAllowlistPrecheckResult { allowlisted: false }),
            )
            .await;
        }
        exec_server_message::Message::ConversationSearchArgs(args) => {
            let tool_call_id = if args.tool_call_id.is_empty() { random_uuid() } else { args.tool_call_id.clone() };
            let error = format!("Conversation search is {NOT_IMPLEMENTED_SUFFIX}");
            let block_args = Map::from_iter([
                ("query".to_owned(), json!(args.query)),
                ("limit".to_owned(), args.limit.map(Value::from).unwrap_or(Value::Null)),
            ]);
            synthesize_cursor_exec_tool_call(output, stream, state, tool_call_id.clone(), "search_conversations", block_args);
            pair_synthesized_exec_result(state, on_tool_result, &tool_call_id, "search_conversations", &error, true).await;
            let exec_result = ConversationSearchResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::conversation_search_result::Result::Error(ConversationSearchError {
                    error,
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::ConversationSearchResult(exec_result)).await;
        }
        exec_server_message::Message::AgentStoreConflictArgs(_) => {
            let exec_result = AgentStoreConflictResult {
                result: Some(super::cursor_agent::r#gen::agent_pb::agent_store_conflict_result::Result::Error(AgentStoreConflictError {
                    error: format!("Agent store conflicts are {NOT_IMPLEMENTED_SUFFIX}"),
                })),
            };
            send_exec_client_message(transport, exec_msg, exec_client_message::Message::AgentStoreConflictResult(exec_result)).await;
        }
        exec_server_message::Message::GitDiffRequest(_) => {
            send_exec_client_throw(transport, exec_msg, &format!("Git diff is {NOT_IMPLEMENTED_SUFFIX}"), Some("exec_variant_unsupported")).await;
        }
        other => {
            let case = exec_server_case_name(other);
            log("warn", Some("unhandledExecMessage"), Some(json!({ "execCase": case })));
            send_exec_client_throw(transport, exec_msg, &format!("No handler for exec message of type {case}"), Some("exec_variant_unsupported")).await;
        }
    }
    Ok(())
}

fn execute_hook_case_name(request: &super::cursor_agent::r#gen::agent_pb::execute_hook_request::Request) -> &'static str {
    use super::cursor_agent::r#gen::agent_pb::execute_hook_request::Request;
    match request {
        Request::PreCompact(_) => "preCompact",
        Request::SubagentStart(_) => "subagentStart",
        Request::SubagentStop(_) => "subagentStop",
        Request::PreToolUse(_) => "preToolUse",
        Request::PostToolUse(_) => "postToolUse",
        Request::PostToolUseFailure(_) => "postToolUseFailure",
        Request::BeforeSubmitPrompt(_) => "beforeSubmitPrompt",
        Request::AfterAgentResponse(_) => "afterAgentResponse",
        Request::AfterAgentThought(_) => "afterAgentThought",
        Request::Stop(_) => "stop",
    }
}

/// Exported for tests: drives one Cursor server message through the stream.
#[allow(clippy::too_many_arguments)]
pub async fn handle_server_message(
    msg: &AgentServerMessage,
    output: &mut AssistantMessage,
    stream: &AssistantMessageEventStream,
    state: &mut BlockState,
    blob_store: &Arc<Mutex<ConversationBlobStore>>,
    transport: &CursorTransport,
    exec_handlers: Option<&CursorExecHandlers>,
    on_tool_result: Option<&CursorToolResultHandler>,
    usage_state: &mut UsageState,
    request_context_tools: &[McpToolDefinition],
    on_conversation_checkpoint: Option<&(dyn Fn(&ConversationStateStructure) + Send + Sync)>,
) {
    let msg_case = msg.message.as_ref();
    log("serverMessage", msg_case.map(server_message_case_name), None);
    match msg_case {
        Some(agent_server_message::Message::InteractionUpdate(update)) => {
            process_interaction_update(update, output, stream, state, usage_state);
        }
        Some(agent_server_message::Message::KvServerMessage(kv_msg)) => {
            handle_kv_server_message(kv_msg, blob_store, transport).await;
        }
        Some(agent_server_message::Message::ExecServerMessage(exec_msg)) => {
            let tracked = stream.track_local_work(handle_exec_server_message(
                exec_msg.clone(),
                transport,
                exec_handlers,
                on_tool_result,
                request_context_tools,
                output,
                stream,
                state,
            ));
            tracked.await;
        }
        Some(agent_server_message::Message::ConversationCheckpointUpdate(checkpoint)) => {
            apply_checkpoint_token_details(checkpoint, output, usage_state);
            if let Some(callback) = on_conversation_checkpoint {
                callback(checkpoint);
            }
        }
        _ => {}
    }
}

fn server_message_case_name(message: &agent_server_message::Message) -> &'static str {
    match message {
        agent_server_message::Message::InteractionUpdate(_) => "interactionUpdate",
        agent_server_message::Message::KvServerMessage(_) => "kvServerMessage",
        agent_server_message::Message::ExecServerMessage(_) => "execServerMessage",
        agent_server_message::Message::ConversationCheckpointUpdate(_) => "conversationCheckpointUpdate",
        _ => "otherServerMessage",
    }
}

async fn handle_kv_server_message(
    kv_msg: &KvServerMessage,
    blob_store: &Arc<Mutex<ConversationBlobStore>>,
    transport: &CursorTransport,
) {
    let Some(kv_case) = kv_msg.message.as_ref() else { return };
    match kv_case {
        kv_server_message::Message::GetBlobArgs(args) => {
            let blob_id_key = hex::encode(&args.blob_id);
            let blob_data = blob_store.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&blob_id_key);
            let response = KvClientMessage {
                id: kv_msg.id,
                message: Some(kv_client_message::Message::GetBlobResult(GetBlobResult {
                    blob_data: blob_data.map(Into::into),
                })),
            };
            let client_message = AgentClientMessage {
                message: Some(agent_client_message::Message::KvClientMessage(response)),
            };
            transport.write_frame(frame_connect_message(&client_message.encode_to_vec(), 0)).await;
            log("kvClient", Some("getBlobResult"), Some(json!({ "blobId": &blob_id_key[..blob_id_key.len().min(40)] })));
        }
        kv_server_message::Message::SetBlobArgs(args) => {
            let blob_id_key = hex::encode(&args.blob_id);
            blob_store
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .set(blob_id_key.clone(), args.blob_data.to_vec());
            let response = KvClientMessage {
                id: kv_msg.id,
                message: Some(kv_client_message::Message::SetBlobResult(SetBlobResult::default())),
            };
            let client_message = AgentClientMessage {
                message: Some(agent_client_message::Message::KvClientMessage(response)),
            };
            transport.write_frame(frame_connect_message(&client_message.encode_to_vec(), 0)).await;
            log("kvClient", Some("setBlobResult"), Some(json!({ "blobId": &blob_id_key[..blob_id_key.len().min(40)] })));
        }
    }
}

// ---------------------------------------------------------------------------
// Model discovery (GetUsableModels)
// ---------------------------------------------------------------------------

const CURSOR_GET_USABLE_MODELS_PATH: &str = "/agent.v1.AgentService/GetUsableModels";
const CURSOR_DEFAULT_CONTEXT_WINDOW: u64 = 200_000;
const CURSOR_DEFAULT_MAX_TOKENS: u64 = 64_000;
const CURSOR_1M_CONTEXT_WINDOW: u64 = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorDiscoveredModel {
    pub id: String,
    pub name: String,
    pub reasoning: bool,
    pub input: Vec<String>,
    pub context_window: u64,
    pub max_tokens: u64,
    pub cursor_max_mode: bool,
}

pub struct FetchCursorUsableModelsOptions {
    pub api_key: String,
    pub base_url: Option<String>,
    pub timeout_ms: Option<u64>,
    pub signal: Option<AbortSignal>,
}

/// Fetches Cursor models through GetUsableModels (unary protobuf over HTTP/2)
/// and normalizes them.
pub async fn fetch_cursor_usable_models(options: FetchCursorUsableModelsOptions) -> Option<Vec<CursorDiscoveredModel>> {
    let timeout_ms = options.timeout_ms.unwrap_or(10_000);
    let base_url = options.base_url.clone().unwrap_or_else(|| CURSOR_API_URL.to_owned());
    let base_url = base_url.trim_end_matches('/').to_owned();

    let request_payload = GetUsableModelsRequest { custom_model_ids: Vec::new() };
    let body = request_payload.encode_to_vec();

    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);
    let response_buffer = tokio::select! {
        _ = &mut deadline => return None,
        _ = async {
            match &options.signal {
                Some(signal) => signal.cancelled().await,
                None => std::future::pending::<()>().await,
            }
        } => return None,
        result = fetch_usable_models_response(&base_url, &options.api_key, &body) => result,
    };
    let response_buffer = response_buffer?;
    if response_buffer.is_empty() {
        return None;
    }
    let decoded = decode_get_usable_models_response(&response_buffer)?;
    let mut by_id: BTreeMap<String, CursorDiscoveredModel> = BTreeMap::new();
    for model in decoded.models {
        let id = model.model_id.trim().to_owned();
        if id.is_empty() {
            continue;
        }
        let name = pick_model_display_name(&model, &id);
        let labeled_1m = has_1m_label(&id)
            || [
                Some(model.display_name.as_str()),
                Some(model.display_name_short.as_str()),
                Some(model.display_model_id.as_str()),
            ]
            .into_iter()
            .flatten()
            .chain(model.aliases.iter().map(String::as_str))
            .any(has_1m_label);
        let max_mode = model.max_mode == Some(true);
        let context_window = if labeled_1m || (max_mode && has_max_mode_1m_family(&id)) {
            CURSOR_1M_CONTEXT_WINDOW
        } else {
            CURSOR_DEFAULT_CONTEXT_WINDOW
        };
        by_id.insert(
            id.clone(),
            CursorDiscoveredModel {
                id: id.clone(),
                name,
                reasoning: model.thinking_details.is_some(),
                input: if has_multimodal_family(&id.to_lowercase()) {
                    vec!["text".to_owned(), "image".to_owned()]
                } else {
                    vec!["text".to_owned()]
                },
                context_window,
                max_tokens: CURSOR_DEFAULT_MAX_TOKENS,
                cursor_max_mode: max_mode,
            },
        );
    }
    Some(by_id.into_values().collect())
}

async fn fetch_usable_models_response(base_url: &str, api_key: &str, body: &[u8]) -> Option<Vec<u8>> {
    let (mut send_request, connection) = open_h2(base_url).await.ok()?;
    let request = http::Request::builder()
        .method("POST")
        .uri(CURSOR_GET_USABLE_MODELS_PATH)
        .header("content-type", "application/proto")
        .header("te", "trailers")
        .header("authorization", format!("Bearer {api_key}"))
        .header("x-ghost-mode", "true")
        .header("x-cursor-client-version", CURSOR_CLIENT_VERSION)
        .header("x-cursor-client-type", "cli")
        .body(())
        .ok()?;
    let (response, mut send_stream) = send_request.send_request(request, false).ok()?;
    let _ = send_stream.send_data(Bytes::copy_from_slice(body), true);
    let response = response.await.ok()?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        connection.abort();
        return None;
    }
    let mut body_stream = response.into_body();
    let mut chunks: Vec<u8> = Vec::new();
    while let Some(chunk) = body_stream.data().await {
        match chunk {
            Ok(chunk) => {
                let _ = body_stream.flow_control().release_capacity(chunk.len());
                chunks.extend_from_slice(&chunk);
            }
            Err(_) => break,
        }
    }
    Some(chunks)
}

fn decode_get_usable_models_response(payload: &[u8]) -> Option<GetUsableModelsResponse> {
    if let Some(framed) = decode_connect_unary_body(payload) {
        return GetUsableModelsResponse::decode(framed.as_slice()).ok();
    }
    GetUsableModelsResponse::decode(payload).ok()
}

fn decode_connect_unary_body(payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() < 5 {
        return None;
    }
    let mut offset = 0;
    while offset + 5 <= payload.len() {
        let flags = payload[offset];
        let message_length =
            u32::from_be_bytes([payload[offset + 1], payload[offset + 2], payload[offset + 3], payload[offset + 4]])
                as usize;
        let frame_end = offset + 5 + message_length;
        if frame_end > payload.len() {
            return None;
        }
        if flags & 0b0000_0001 != 0 {
            return None;
        }
        if flags & 0b0000_0010 == 0 {
            return Some(payload[offset + 5..frame_end].to_vec());
        }
        offset = frame_end;
    }
    None
}

fn has_1m_label(value: &str) -> bool {
    value.split(|c: char| !c.is_alphanumeric()).any(|token| token.eq_ignore_ascii_case("1m"))
}

fn has_max_mode_1m_family(id: &str) -> bool {
    id.contains("claude") || id.contains("gemini")
}

fn has_multimodal_family(id: &str) -> bool {
    id.contains("claude") || id.contains("gemini") || id.contains("gpt-") || id.contains("codex")
}

fn pick_model_display_name(
    model: &ModelDetails,
    fallback_id: &str,
) -> String {
    for candidate in [
        Some(model.display_name.as_str()),
        Some(model.display_name_short.as_str()),
        Some(model.display_model_id.as_str()),
    ]
    .into_iter()
    .flatten()
    .chain(model.aliases.iter().map(String::as_str))
    {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            return trimmed.to_owned();
        }
    }
    fallback_id.to_owned()
}

// ---------------------------------------------------------------------------
// Run stream
// ---------------------------------------------------------------------------

struct PinnedModels {
    requested_model: Option<RequestedModel>,
    model_details: Option<ModelDetails>,
}

struct StreamFailure {
    message: String,
    retryable: Option<CursorStreamRetryCause>,
}

struct AttemptOutcome {}

struct AttemptContext<'a> {
    model: &'a Model,
    context: &'a Context,
    options: &'a CursorAgentOptions,
    stream: &'a AssistantMessageEventStream,
    output: &'a mut AssistantMessage,
    state: &'a mut BlockState,
    usage_state: &'a mut UsageState,
    base_conversation_id: &'a mut Option<String>,
    conversation_id: &'a mut Option<String>,
    pinned: &'a mut PinnedModels,
    force_resume_action: bool,
    attempt: u32,
    live_key: &'a mut Option<String>,
    pinned_store: &'a mut Option<Arc<Mutex<ConversationBlobStore>>>,
}

async fn run_attempt(ctx: &mut AttemptContext<'_>) -> Result<AttemptOutcome, StreamFailure> {
    let api_key = ctx.options.base.request.api_key.clone().ok_or_else(|| StreamFailure {
        message: "Cursor access token is required; run /login cursor".to_owned(),
        retryable: None,
    })?;

    let base_conversation_id = ctx
        .options
        .conversation_id
        .clone()
        .or_else(|| ctx.options.base.session_id.clone())
        .unwrap_or_else(random_uuid);
    let conversation_id = {
        let holder = rotation_store();
        holder.store.get_wire_id(&base_conversation_id)
    };
    *ctx.base_conversation_id = Some(base_conversation_id.clone());
    *ctx.conversation_id = Some(conversation_id.clone());
    register_conversation_cache_key(ctx.options.base.session_id.as_deref(), &conversation_id);
    *ctx.live_key = Some(conversation_id.clone());
    retain_live_conversation(&conversation_id);
    let blob_store = get_or_create_conversation_blob_store(&conversation_id, ctx.options.base.session_id.as_deref());
    *ctx.pinned_store = Some(blob_store.clone());
    blob_store.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).begin_request_pins();

    let cached_state = conversations().state.get(&conversation_id).cloned();
    let request_output = build_grpc_request(
        ctx.model,
        ctx.context,
        Some(ctx.options),
        GrpcRequestState {
            conversation_id: conversation_id.clone(),
            blob_store: blob_store.clone(),
            conversation_state: cached_state,
            force_resume_action: ctx.force_resume_action,
            pinned_requested_model: ctx.pinned.requested_model.clone(),
            pinned_model_details: ctx.pinned.model_details.clone(),
        },
    )
    .map_err(|message| StreamFailure { message, retryable: None })?;
    if ctx.pinned.requested_model.is_none() {
        ctx.pinned.requested_model = Some(request_output.requested_model.clone());
    }
    if ctx.pinned.model_details.is_none() {
        ctx.pinned.model_details = Some(request_output.model_details.clone());
    }
    conversations().state.insert(conversation_id.clone(), request_output.conversation_state.clone());
    enforce_conversation_total_blob_limit();

    let request_context_tools = build_mcp_tool_definitions(ctx.context.tools.as_deref());

    let base_url = if ctx.model.base_url.is_empty() { CURSOR_API_URL.to_owned() } else { ctx.model.base_url.clone() };
    let caller_headers = sanitize_cursor_caller_headers(
        provider_headers_to_record(ctx.options.base.request.headers.as_ref()).as_ref(),
    );

    let (mut send_request, connection) = open_h2(&base_url).await.map_err(|message| StreamFailure {
        message,
        retryable: Some(CursorStreamRetryCause::Transport),
    })?;
    let request = build_run_request_headers(&base_url, &api_key, &caller_headers)
        .map_err(|message| StreamFailure { message, retryable: None })?;
    let (response, send_stream) = send_request.send_request(request, false).map_err(|error| StreamFailure {
        message: error.to_string(),
        retryable: Some(CursorStreamRetryCause::Transport),
    })?;
    let transport = CursorTransport {
        send: Arc::new(tokio::sync::Mutex::new(Some(send_stream))),
        closed: Arc::new(AtomicBool::new(false)),
    };

    let response = match response.await {
        Ok(response) => response,
        Err(error) => {
            connection.abort();
            transport.close();
            let message = map_h2_transport_error(None, &error.to_string(), &base_url);
            return Err(StreamFailure { message, retryable: Some(CursorStreamRetryCause::Transport) });
        }
    };
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        connection.abort();
        transport.close();
        return Err(StreamFailure {
            message: format!("Cursor run request failed with status {status}"),
            retryable: None,
        });
    }

    if ctx.attempt == 1 {
        ctx.stream.push(AssistantMessageEvent::Start { partial: ctx.output.clone() });
    }

    let mut body = response.into_body();
    let health_fail_threshold_ms =
        ctx.options.stream_health_fail_threshold_ms.unwrap_or(CURSOR_STREAM_HEALTH_FAIL_THRESHOLD_MS);

    let heartbeat = {
        let transport = transport.clone();
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_millis(5000));
            ticker.tick().await;
            loop {
                ticker.tick().await;
                if transport.is_closed() {
                    return;
                }
                let heartbeat_message = AgentClientMessage {
                    message: Some(agent_client_message::Message::ClientHeartbeat(ClientHeartbeat::default())),
                };
                transport.write_frame(frame_connect_message(&heartbeat_message.encode_to_vec(), 0)).await;
            }
        })
    };

    transport.write_frame(frame_connect_message(&request_output.request_bytes, 0)).await;

    let mut pending: Vec<u8> = Vec::new();
    let mut end_stream_error: Option<String> = None;
    let mut saw_turn_ended = false;
    let abort_signal = ctx.options.base.request.signal.clone();

    let outcome: Result<AttemptOutcome, StreamFailure> = loop {
        if saw_turn_ended {
            break Ok(AttemptOutcome {});
        }
        let deadline = tokio::time::sleep(Duration::from_millis(health_fail_threshold_ms));
        tokio::pin!(deadline);
        let next = tokio::select! {
            biased;
            _ = async {
                match &abort_signal {
                    Some(signal) => signal.cancelled().await,
                    None => std::future::pending::<()>().await,
                }
            } => {
                break Err(StreamFailure { message: "Request was aborted".to_owned(), retryable: None });
            }
            _ = &mut deadline => {
                break Err(StreamFailure {
                    message: "Cursor stream ended before turnEnded: inbound stream stalled".to_owned(),
                    retryable: Some(CursorStreamRetryCause::Stall),
                });
            }
            chunk = body.data() => chunk,
        };
        let Some(chunk) = next else {
            break Err(StreamFailure {
                message: "Cursor stream ended before turnEnded".to_owned(),
                retryable: Some(CursorStreamRetryCause::CleanEnd),
            });
        };
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(error) => {
                break Err(StreamFailure {
                    message: error.to_string(),
                    retryable: Some(CursorStreamRetryCause::Transport),
                });
            }
        };
        let _ = body.flow_control().release_capacity(chunk.len());
        pending.extend_from_slice(&chunk);

        while pending.len() >= 5 {
            let flags = pending[0];
            let message_length = u32::from_be_bytes([pending[1], pending[2], pending[3], pending[4]]) as usize;
            if pending.len() < 5 + message_length {
                break;
            }
            let message_bytes = pending[5..5 + message_length].to_vec();
            pending.drain(..5 + message_length);

            if flags & CONNECT_END_STREAM_FLAG != 0 {
                if let Some(error) = parse_connect_end_stream(&message_bytes) {
                    end_stream_error = Some(error);
                    transport.close();
                }
                continue;
            }

            let Ok(server_message) = AgentServerMessage::decode(message_bytes.as_slice()) else {
                continue;
            };
            let is_turn_ended = matches!(
                server_message.message.as_ref(),
                Some(agent_server_message::Message::InteractionUpdate(update))
                    if matches!(update.message.as_ref(), Some(interaction_update::Message::TurnEnded(_)))
            );
            if let Some(agent_server_message::Message::ConversationCheckpointUpdate(checkpoint)) =
                server_message.message.as_ref()
            {
                let max_tokens = checkpoint.token_details.as_ref().map(|details| details.max_tokens as f64);
                record_cursor_context_limit(&ctx.model.id, max_tokens);
            }
            let on_tool_result = ctx.state.on_tool_result.clone();
            handle_server_message(
                &server_message,
                ctx.output,
                ctx.stream,
                ctx.state,
                &blob_store,
                &transport,
                ctx.options.exec_handlers.as_ref(),
                on_tool_result.as_ref(),
                ctx.usage_state,
                &request_context_tools,
                None,
            )
            .await;
            if is_turn_ended {
                saw_turn_ended = true;
                break;
            }
        }
    };

    heartbeat.abort();
    transport.close();

    match outcome {
        Ok(outcome) => Ok(outcome),
        Err(failure) => {
            if let Some(end_stream_error) = end_stream_error {
                return Err(StreamFailure { message: end_stream_error, retryable: None });
            }
            Err(failure)
        }
    }
}

async fn run_stream(
    model: Model,
    context: Context,
    options: CursorAgentOptions,
    stream: AssistantMessageEventStream,
) {
    let mut output = AssistantMessage {
        content: Vec::new(),
        api: model.api.clone(),
        provider: model.provider.clone(),
        model: model.id.clone(),
        response_model: None,
        response_id: None,
        provider_thinking_level: None,
        diagnostics: None,
        usage: Usage::default(),
        stop_reason: crate::types::StopReason::Stop,
        stop_details: None,
        deferred: None,
        error_message: None,
        abort_source: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    };

    let mut state = BlockState {
        current_text_block: None,
        current_text_index: None,
        current_thinking_block: None,
        current_thinking_index: None,
        current_tool_call: None,
        open_tool_calls: HashMap::new(),
        resolved_mcp_tool_call_ids: HashSet::new(),
        streaming: HashMap::new(),
        on_tool_result: options
            .on_tool_result
            .clone()
            .or_else(|| options.exec_handlers.as_ref().and_then(|handlers| handlers.on_tool_result.clone())),
    };

    let mut base_conversation_id: Option<String> = None;
    let mut conversation_id: Option<String> = None;
    let mut usage_state = UsageState::default();
    let mut pinned = PinnedModels { requested_model: None, model_details: None };
    let mut attempt = 0u32;
    let mut stream_retries = 0u32;
    let mut force_resume_action = false;

    loop {
        attempt += 1;
        let mut live_key: Option<String> = None;
        let mut pinned_store: Option<Arc<Mutex<ConversationBlobStore>>> = None;
        let outcome = {
            let mut ctx = AttemptContext {
                model: &model,
                context: &context,
                options: &options,
                stream: &stream,
                output: &mut output,
                state: &mut state,
                usage_state: &mut usage_state,
                base_conversation_id: &mut base_conversation_id,
                conversation_id: &mut conversation_id,
                pinned: &mut pinned,
                force_resume_action,
                attempt,
                live_key: &mut live_key,
                pinned_store: &mut pinned_store,
            };
            run_attempt(&mut ctx).await
        };

        if let Some(store) = &pinned_store {
            store.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).release_request_pins();
        }
        if let Some(key) = &live_key {
            release_live_conversation(key);
        }
        if pinned_store.is_some() {
            enforce_conversation_total_blob_limit();
        }

        match outcome {
            Ok(_) => {
                end_current_text_block(&mut output, &stream, &mut state);
                end_current_thinking_block(&mut output, &stream, &mut state);
                flush_open_tool_calls(&mut output, &stream, &mut state);
                let _ = calculate_cost(&model, &mut output.usage);
                let reason = match output.stop_reason {
                    crate::types::StopReason::Length => DoneReason::Length,
                    crate::types::StopReason::ToolUse => DoneReason::ToolUse,
                    _ => DoneReason::Stop,
                };
                stream.push(AssistantMessageEvent::Done { reason, message: output.clone() });
                stream.end(None);
                return;
            }
            Err(failure) => {
                let aborted = options.base.request.signal.as_ref().is_some_and(AbortSignal::aborted);
                let should_retry = failure.retryable.is_some()
                    && !aborted
                    && stream_retries < options.stream_stall_max_retries.unwrap_or(10);
                if should_retry {
                    force_resume_action = force_resume_action || pinned.requested_model.is_some();
                    let delay = cursor_stream_retry_delay_ms(
                        super::cursor_agent::stream_retry::CursorStreamRetryDelayOptions {
                            attempt: stream_retries,
                            base_delay_ms: None,
                            fixed_delay_ms: options.stream_stall_retry_delay_ms,
                        },
                        rand_f64,
                    );
                    stream_retries += 1;
                    wait_for_cursor_stream_retry(delay, options.base.request.signal.as_ref()).await;
                    if options.base.request.signal.as_ref().is_some_and(AbortSignal::aborted) {
                        output.stop_reason = crate::types::StopReason::Aborted;
                        output.error_message = Some("Request was aborted".to_owned());
                        stream.push(AssistantMessageEvent::Error {
                            reason: ErrorReason::Aborted,
                            error: output.clone(),
                        });
                        stream.end(None);
                        return;
                    }
                    continue;
                }

                let mut message = failure.message;
                let zero_token_resource_exhausted = is_zero_token_resource_exhausted(&message, usage_state.saw_token_delta);
                if zero_token_resource_exhausted
                    && let (Some(current), Some(base)) =
                        (conversation_id.clone(), base_conversation_id.clone())
                {
                    let holder = rotation_store();
                    if holder.store.should_skip(&base) {
                        message = CURSOR_CONVERSATION_POISONED_MESSAGE.to_owned();
                    } else if holder.store.should_surface_before_rotating(&base) {
                        holder.store.mark_surfaced(&base, &current);
                    } else {
                        let holder = rotation_store();
                        match holder.store.record_zero_token_poison(&base, &current) {
                            PoisonDecision::Rotated { wire_id } => {
                                if let Some(cached) = conversations().state.get(&current).cloned() {
                                    conversations().state.insert(wire_id.clone(), cached);
                                }
                                if let Some(blobs) = conversations().blobs.get(&current).cloned() {
                                    conversations().blobs.insert(wire_id.clone(), blobs);
                                }
                                register_conversation_cache_key(options.base.session_id.as_deref(), &wire_id);
                                enforce_conversation_cache_limit(&wire_id, options.base.session_id.as_deref());
                                if wire_id != current {
                                    forget_conversation_cache_key(&current);
                                }
                                drop(holder);
                                continue;
                            }
                            PoisonDecision::Exhausted => {
                                message = CURSOR_CONVERSATION_POISONED_MESSAGE.to_owned();
                            }
                        }
                    }
                }

                output.stop_reason = if aborted {
                    crate::types::StopReason::Aborted
                } else {
                    crate::types::StopReason::Error
                };
                output.error_message = Some(message);
                let reason = if aborted { ErrorReason::Aborted } else { ErrorReason::Error };
                stream.push(AssistantMessageEvent::Error { reason, error: output.clone() });
                stream.end(None);
                return;
            }
        }
    }
}

fn rand_f64() -> f64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::time::SystemTime::now().hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    (hasher.finish() % 1_000_000) as f64 / 1_000_000.0
}

pub fn stream_with_options(
    model: &Model,
    context: &Context,
    options: Option<CursorAgentOptions>,
) -> AssistantMessageEventStream {
    let stream = AssistantMessageEventStream::assistant();
    let model = model.clone();
    let context = context.clone();
    let options = options.unwrap_or_default();
    let out = stream.clone();
    tokio::spawn(async move { run_stream(model, context, options, out).await });
    stream
}

pub fn stream(model: &Model, context: &Context, options: Option<StreamOptions>) -> AssistantMessageEventStream {
    stream_with_options(model, context, options.map(cursor_options_from_stream_options))
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let mut cursor_options = options
        .as_ref()
        .map(|options| cursor_options_from_stream_options(options.stream.clone()))
        .unwrap_or_default();
    if let Some(options) = options {
        cursor_options.thinking_selection = options.thinking_selection;
    }
    stream_with_options(model, context, Some(cursor_options))
}

fn cursor_options_from_stream_options(base: StreamOptions) -> CursorAgentOptions {
    CursorAgentOptions { base, ..CursorAgentOptions::default() }
}

pub struct CursorAgentStreams;

impl crate::types::ProviderStreams for CursorAgentStreams {
    fn stream(
        &self,
        model: &Model,
        context: &Context,
        options: Option<StreamOptions>,
    ) -> AssistantMessageEventStream {
        stream(model, context, options)
    }

    fn stream_simple(
        &self,
        model: &Model,
        context: &Context,
        options: Option<SimpleStreamOptions>,
    ) -> AssistantMessageEventStream {
        stream_simple(model, context, options)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{InputModality, ModelCost, ModelThinkingLevel, ThinkingSelection, ThinkingSelectionSource};
    use serde_json::json;

    /// The generic `TResult` in `resolve_exec_handler` must be able to describe
    /// itself; tests use a plain string, whose description is the string.
    impl DescribeExecResult for String {
        fn describe_exec_result(&self) -> (String, bool) {
            (self.clone(), false)
        }
    }

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::text(text)
    }

    fn tool_result_message(
        tool_call_id: &str,
        tool_name: &str,
        content: &str,
        is_error: bool,
        details: Option<Value>,
    ) -> ToolResultMessage {
        ToolResultMessage {
            tool_call_id: tool_call_id.to_owned(),
            tool_name: tool_name.to_owned(),
            content: vec![text_block(content)],
            details,
            usage: None,
            added_tool_names: None,
            is_error,
            timestamp: 0,
        }
    }

    fn tool(name: &str, description: &str, parameters: Value) -> Tool {
        Tool {
            name: name.to_owned(),
            description: description.to_owned(),
            parameters,
            freeform: None,
            constrained_sampling: None,
        }
    }

    fn model(id: &str, compat: Option<Value>) -> Model {
        Model {
            id: id.to_owned(),
            name: id.to_owned(),
            api: "cursor-agent".to_owned(),
            provider: "cursor".to_owned(),
            base_url: "https://api2.cursor.sh".to_owned(),
            reasoning: true,
            thinking_level_map: None,
            input: vec![InputModality::Text],
            cost: ModelCost::default(),
            context_window: 300_000,
            max_tokens: 64_000,
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: compat.map(|value| serde_json::from_value(value).expect("compat")),
        }
    }

    fn assistant_for_tests() -> AssistantMessage {
        AssistantMessage {
            content: Vec::new(),
            api: "cursor-agent".to_owned(),
            provider: "cursor".to_owned(),
            model: "m".to_owned(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: crate::types::StopReason::Stop,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    fn assistant_with_tool_call_ids(ids: &[&str]) -> AssistantMessage {
        let mut message = assistant_for_tests();
        message.content = ids
            .iter()
            .map(|id| {
                ContentBlock::ToolCall(ToolCall {
                    id: (*id).to_owned(),
                    name: "read".to_owned(),
                    arguments: Map::new(),
                    incomplete: None,
                    error_message: None,
                    thought_signature: None,
                    namespace: None,
                })
            })
            .collect();
        message
    }

    fn empty_block_state() -> BlockState {
        BlockState {
            current_text_block: None,
            current_text_index: None,
            current_thinking_block: None,
            current_thinking_index: None,
            current_tool_call: None,
            open_tool_calls: HashMap::new(),
            resolved_mcp_tool_call_ids: HashSet::new(),
            streaming: HashMap::new(),
            on_tool_result: None,
        }
    }

    #[test]
    fn merges_streamed_and_completion_mcp_args_per_key() {
        let streamed = json!({ "tasks": [{ "id": 1 }], "name": "streamed", "keep": "streamed-only" });
        let completion = json!({ "tasks": "[object Object]", "name": "completion" });
        let merged = merge_cursor_mcp_tool_call_args(streamed.as_object(), completion.as_object());
        assert_eq!(
            Value::Object(merged),
            json!({
                "tasks": [{ "id": 1 }],
                "name": "completion",
                "keep": "streamed-only"
            })
        );
    }

    #[test]
    fn rejects_empty_grep_patterns_with_a_glob_aware_hint() {
        assert_eq!(empty_grep_pattern_rejection(Some("real-pattern"), None), None);
        let hinted = empty_grep_pattern_rejection(Some(""), Some("*.ts")).expect("hint");
        assert!(hinted.contains("\"*.ts\""), "{hinted}");
        let plain = empty_grep_pattern_rejection(Some("  "), None).expect("plain");
        assert!(plain.contains("pattern is required"), "{plain}");
    }

    #[test]
    fn builds_root_prompt_history_with_paired_tool_calls_and_results() {
        let messages = vec![
            Message::User(crate::types::UserMessage {
                content: crate::types::UserContent::Text("read the file".to_owned()),
                timestamp: 0,
            }),
            Message::Assistant(Box::new(AssistantMessage {
                content: vec![
                    text_block("reading"),
                    ContentBlock::ToolCall(ToolCall {
                        id: "call-1".to_owned(),
                        name: "read".to_owned(),
                        arguments: Map::from_iter([("path".to_owned(), json!("a.ts"))]),
                        incomplete: None,
                        error_message: None,
                        thought_signature: None,
                        namespace: None,
                    }),
                ],
                ..assistant_for_tests()
            })),
            Message::ToolResult(tool_result_message("call-1", "read", "contents", false, None)),
            Message::User(crate::types::UserMessage {
                content: crate::types::UserContent::Text("now summarize".to_owned()),
                timestamp: 0,
            }),
        ];

        let history = build_cursor_history_for_test(&messages, None).expect("history");
        assert_eq!(
            history.root_prompt_messages_json,
            vec![
                json!({ "role": "user", "content": [{ "type": "text", "text": "read the file" }] }),
                json!({
                    "role": "assistant",
                    "content": [
                        { "type": "text", "text": "reading" },
                        { "type": "tool-call", "toolCallId": "call-1", "toolName": "read", "args": { "path": "a.ts" } }
                    ]
                }),
                json!({
                    "role": "tool",
                    "id": "call-1",
                    "content": [{ "type": "tool-result", "toolName": "read", "toolCallId": "call-1", "result": "contents" }]
                }),
            ]
        );
        assert_eq!(history.turn_user_messages_json.len(), 1);
        assert_eq!(history.turn_user_messages_json[0]["text"], json!("read the file"));
        assert_eq!(history.turn_step_messages_json[0].len(), 2);
    }

    #[test]
    fn keeps_orphan_tool_results_as_bracketed_assistant_steps() {
        let messages = vec![
            Message::User(crate::types::UserMessage {
                content: crate::types::UserContent::Text("hi".to_owned()),
                timestamp: 0,
            }),
            Message::ToolResult(tool_result_message("orphan-1", "bash", "stray output", true, None)),
            Message::User(crate::types::UserMessage {
                content: crate::types::UserContent::Text("next".to_owned()),
                timestamp: 0,
            }),
        ];
        let history = build_cursor_history_for_test(&messages, None).expect("history");
        let rendered = serde_json::to_string(&history.turn_step_messages_json).expect("json");
        assert!(rendered.contains("[Tool Error]"), "{rendered}");
    }

    #[test]
    fn builds_one_default_system_prompt_blob_when_none_is_provided() {
        assert_eq!(
            build_cursor_system_prompt_jsons(None, None),
            vec![json!({ "role": "system", "content": "You are a helpful assistant." }).to_string()]
        );
        assert_eq!(
            build_cursor_system_prompt_jsons(Some("Be terse."), None),
            vec![json!({ "role": "system", "content": "Be terse." }).to_string()]
        );
    }

    #[test]
    fn filters_cursor_native_tools_out_of_the_mcp_catalog() {
        let tools = vec![
            tool("bash", "", json!({ "type": "object" })),
            tool("read", "", json!({ "type": "object" })),
            tool("edit", "edits", json!({ "type": "object", "properties": { "path": { "type": "string" } } })),
        ];
        let definitions = build_mcp_tool_definitions(Some(&tools));
        assert_eq!(definitions.iter().map(|d| d.name.clone()).collect::<Vec<_>>(), vec!["edit".to_owned()]);
        assert_eq!(definitions[0].provider_identifier, "pi-agent");
        assert_eq!(definitions[0].tool_name, "edit");
    }

    #[test]
    fn sanitizes_caller_headers_for_http2() {
        let headers = BTreeMap::from([
            (":authority".to_owned(), "x".to_owned()),
            ("Connection".to_owned(), "keep-alive".to_owned()),
            ("AUTHORIZATION".to_owned(), "Bearer nope".to_owned()),
            ("host".to_owned(), "evil".to_owned()),
            ("content-length".to_owned(), "5".to_owned()),
            ("X-Custom".to_owned(), "keep".to_owned()),
        ]);
        assert_eq!(
            sanitize_cursor_caller_headers(Some(&headers)),
            BTreeMap::from([("x-custom".to_owned(), "keep".to_owned())])
        );
    }

    #[test]
    fn frames_connect_messages_with_a_five_byte_prefix() {
        assert_eq!(frame_connect_message(&[1, 2, 3], 0), vec![0, 0, 0, 0, 3, 1, 2, 3]);
        assert_eq!(frame_connect_message(&[], 2), vec![2, 0, 0, 0, 0]);
    }

    #[test]
    fn parses_connect_end_stream_errors() {
        let payload = br#"{"error":{"code":"resource_exhausted","message":"quota exceeded"}}"#;
        assert_eq!(
            parse_connect_end_stream(payload),
            Some("Connect error resource_exhausted: quota exceeded".to_owned())
        );
        assert_eq!(parse_connect_end_stream(b"not json"), Some("Failed to parse Connect end stream".to_owned()));
        assert_eq!(parse_connect_end_stream(br#"{}"#), None);
    }

    #[test]
    fn maps_h2_alpn_failures_into_actionable_errors() {
        let message = map_h2_transport_error(
            Some("ERR_HTTP2_ERROR"),
            "h2 is not supported by this server",
            "https://api2.cursor.sh",
        );
        assert!(message.contains("could not negotiate HTTP/2"), "{message}");
        assert!(message.contains("ALPN"), "{message}");
        assert_eq!(map_h2_transport_error(None, "boom", "https://x"), "boom");
    }

    #[test]
    fn builds_a_read_exec_result_envelope_from_a_tool_result() {
        let ok = build_read_result_from_tool_result("a.ts", &tool_result_message("c", "read", "one\ntwo", false, None), false);
        match ok.result {
            Some(read_result::Result::Success(success)) => {
                assert_eq!(success.path, "a.ts");
                assert_eq!(success.total_lines, 2);
                assert_eq!(success.file_size, 7);
                assert!(!success.range_applied);
                assert!(matches!(success.output, Some(read_success::Output::Content(ref text)) if text == "one\ntwo"));
            }
            other => panic!("expected success, got {other:?}"),
        }

        let windowed = build_read_result_from_tool_result("a.ts", &tool_result_message("c", "read", "one\ntwo", false, None), true);
        match windowed.result {
            Some(read_result::Result::Success(success)) => {
                assert_eq!(success.total_lines, 0);
                assert!(success.range_applied);
            }
            other => panic!("expected success, got {other:?}"),
        }

        let failed = build_read_result_from_tool_result("a.ts", &tool_result_message("c", "read", "boom", true, None), false);
        assert!(matches!(
            failed.result,
            Some(read_result::Result::Error(ReadError { ref path, ref error })) if path == "a.ts" && error == "boom"
        ));
    }

    #[test]
    fn ensure_unique_tool_call_id_keeps_mints_and_suffixes() {
        let empty = assistant_for_tests();

        let mut kept = Some("StrReplace_0_aa-1".to_owned());
        ensure_unique_cursor_exec_tool_call_id(&empty, &mut kept);
        assert_eq!(kept.as_deref(), Some("StrReplace_0_aa-1"));

        let mut minted: Option<String> = None;
        ensure_unique_cursor_exec_tool_call_id(&empty, &mut minted);
        assert!(minted.as_deref().is_some_and(|id| !id.is_empty()));

        let used = assistant_with_tool_call_ids(&["StrReplace_0_aa-1"]);
        let mut suffixed = Some("StrReplace_0_aa-1".to_owned());
        ensure_unique_cursor_exec_tool_call_id(&used, &mut suffixed);
        assert_eq!(suffixed.as_deref(), Some("StrReplace_0_aa-1-2"));

        let taken = assistant_with_tool_call_ids(&["id-1", "id-1-2"]);
        let mut skipped = Some("id-1".to_owned());
        ensure_unique_cursor_exec_tool_call_id(&taken, &mut skipped);
        assert_eq!(skipped.as_deref(), Some("id-1-3"));
    }

    #[tokio::test]
    async fn resolve_exec_handler_rejects_and_pairs_without_a_handler() {
        let paired = Arc::new(Mutex::new(Vec::new()));
        let sink_paired = paired.clone();
        let on_tool_result: CursorToolResultHandler = Arc::new(move |result| {
            let sink = sink_paired.clone();
            Box::pin(async move {
                sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(result);
                None
            })
        });
        let pairing = CursorExecPairing { tool_call_id: "id-1".to_owned(), tool_name: "read".to_owned() };
        let (exec_result, tool_result) = resolve_exec_handler::<(), String>(
            (),
            None,
            Some(&on_tool_result),
            |tool_result| format!("from-tool-result:{}", tool_result.content.len()),
            |reason| format!("rejected:{reason}"),
            |error| format!("error:{error}"),
            Some(&pairing),
        )
        .await;
        assert_eq!(exec_result, "rejected:Tool not available");
        assert_eq!(tool_result.map(|result| result.is_error), Some(true));
        assert_eq!(paired.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len(), 1);
    }

    #[tokio::test]
    async fn resolve_exec_handler_derives_the_wire_result_from_a_tool_result_message() {
        let returned = tool_result_message("id-1", "read", "ok", false, None);
        let expected = returned.clone();
        let handler: Arc<dyn Fn(()) -> crate::api::cursor_agent::types::ExecFuture<String> + Send + Sync> =
            Arc::new(move |_args| {
                let returned = returned.clone();
                Box::pin(async move { CursorExecHandlerResult::ToolResult(returned) })
            });
        let pairing = CursorExecPairing { tool_call_id: "id-1".to_owned(), tool_name: "read".to_owned() };
        let (exec_result, tool_result) = resolve_exec_handler::<(), String>(
            (),
            Some(&handler),
            None,
            |tool_result| format!("from-tool-result:{}", tool_result.content.len()),
            |reason| format!("rejected:{reason}"),
            |error| format!("error:{error}"),
            Some(&pairing),
        )
        .await;
        assert_eq!(exec_result, "from-tool-result:1");
        assert_eq!(tool_result, Some(expected));
    }

    #[tokio::test]
    async fn resolve_exec_handler_lets_the_transformer_rewrite_the_paired_result() {
        let returned = tool_result_message("id-1", "read", "original", false, None);
        let handler: Arc<dyn Fn(()) -> crate::api::cursor_agent::types::ExecFuture<String> + Send + Sync> =
            Arc::new(move |_args| {
                let returned = returned.clone();
                Box::pin(async move { CursorExecHandlerResult::ToolResult(returned) })
            });
        let on_tool_result: CursorToolResultHandler = Arc::new(|mut result| {
            Box::pin(async move {
                result.content = vec![text_block("rewritten")];
                Some(result)
            })
        });
        let pairing = CursorExecPairing { tool_call_id: "id-1".to_owned(), tool_name: "read".to_owned() };
        let (_exec_result, tool_result) = resolve_exec_handler::<(), String>(
            (),
            Some(&handler),
            Some(&on_tool_result),
            |tool_result| format!("from-tool-result:{}", tool_result.content.len()),
            |reason| format!("rejected:{reason}"),
            |error| format!("error:{error}"),
            Some(&pairing),
        )
        .await;
        let tool_result = tool_result.expect("paired");
        assert_eq!(tool_result.content, vec![text_block("rewritten")]);
    }

    #[test]
    fn sanitizes_tool_schemas_by_stripping_composition_keywords() {
        let schema = json!({
            "type": "object",
            "properties": {
                "ruleFile": { "type": "string", "minLength": 1 },
                "inlineRules": { "type": "string" }
            },
            "required": ["ruleFile"],
            "oneOf": [
                { "type": "object", "required": ["ruleFile"], "not": { "required": ["inlineRules"] } },
                { "type": "object", "required": ["inlineRules"], "not": { "required": ["ruleFile"] } }
            ]
        });
        let sanitized = sanitize_cursor_tool_schema(&schema);
        assert_eq!(sanitized.get("oneOf"), None);
        assert_eq!(sanitized["type"], json!("object"));
        assert_eq!(sanitized["properties"]["ruleFile"], json!({ "type": "string", "minLength": 1 }));
        assert_eq!(sanitized["required"], json!(["ruleFile"]));
    }

    #[test]
    fn sanitizes_nested_composition_keywords_inside_property_schemas() {
        let schema = json!({
            "type": "object",
            "properties": {
                "filter": { "type": "object", "properties": { "kind": { "type": "string" } }, "anyOf": [{ "required": ["kind"] }] },
                "list": { "type": "array", "items": { "type": "object", "allOf": [{ "required": ["x"] }] } }
            }
        });
        let sanitized = sanitize_cursor_tool_schema(&schema);
        assert_eq!(sanitized["properties"]["filter"].get("anyOf"), None);
        assert_eq!(sanitized["properties"]["filter"]["properties"]["kind"], json!({ "type": "string" }));
        assert_eq!(sanitized["properties"]["list"]["items"].get("allOf"), None);
    }

    #[test]
    fn passes_clean_schemas_through_unchanged() {
        let parameters = json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "maxLength": 16384 },
                "lang": { "enum": ["ts", "py"] },
                "not": { "required": ["other"] }
            },
            "required": ["pattern"],
            "additionalProperties": false
        });
        assert_eq!(sanitize_cursor_tool_schema(&parameters), parameters);
    }

    #[test]
    fn reports_the_conversation_cache_shape() {
        let stats = get_cursor_conversation_cache_stats();
        assert_eq!(stats.keys.len(), stats.conversations);
        assert_eq!(stats.blob_bytes, 0);
    }

    #[test]
    fn renders_an_absent_selection_as_the_representative_variant() {
        let model = model(
            "gpt-5.5-medium",
            Some(json!({ "cursorReasoning": { "capabilityId": "gpt-5.5", "representativeVariantId": "gpt-5.5-medium" } })),
        );
        let requested = build_requested_model(&model, None);
        assert_eq!(requested.model_id, "gpt-5.5-medium");
        assert!(requested.parameters.is_empty());
        assert!(!requested.max_mode);
    }

    #[test]
    fn renders_an_explicit_selection_as_the_catalog_suffix_variant() {
        let model = model(
            "gpt-5.5",
            Some(json!({ "cursorReasoning": { "capabilityId": "gpt-5.5", "representativeVariantId": "gpt-5.5-medium" } })),
        );
        let selection = ThinkingSelection {
            level: ModelThinkingLevel::Xhigh,
            source: ThinkingSelectionSource::Explicit,
            legacy_variant_id: None,
        };
        let requested = build_requested_model(&model, Some(&selection));
        assert_eq!(requested.model_id, "gpt-5.5-extra-high");
        assert!(requested.parameters.is_empty());
    }

    #[test]
    fn keeps_max_mode_orthogonal_to_the_suffix_variant_id() {
        let model = model(
            "",
            Some(json!({
                "cursorMaxMode": true,
                "cursorReasoning": { "capabilityId": "", "thinkingMode": false, "representativeVariantId": "" }
            })),
        );
        let selection = ThinkingSelection {
            level: ModelThinkingLevel::High,
            source: ThinkingSelectionSource::Explicit,
            legacy_variant_id: None,
        };
        let requested = build_requested_model(&model, Some(&selection));
        assert!(requested.max_mode);
        assert_eq!(requested.model_id, "");
        assert!(requested.parameters.is_empty());
    }

    #[test]
    fn builds_the_run_request_with_a_user_message_action() {
        let model = model("gpt-5.5-medium", None);
        let context = Context {
            system_prompt: Some("Be terse.".to_owned()),
            messages: vec![Message::User(crate::types::UserMessage {
                content: crate::types::UserContent::Text("hello".to_owned()),
                timestamp: 0,
            })],
            tools: None,
        };
        let output = build_grpc_request(
            &model,
            &context,
            None,
            GrpcRequestState {
                conversation_id: "conv-1".to_owned(),
                blob_store: Arc::new(Mutex::new(ConversationBlobStore::new())),
                conversation_state: None,
                force_resume_action: false,
                pinned_requested_model: None,
                pinned_model_details: None,
            },
        )
        .expect("request");
        let message = AgentClientMessage::decode(output.request_bytes.as_slice()).expect("decode");
        let Some(agent_client_message::Message::RunRequest(request)) = message.message else {
            panic!("expected a run request");
        };
        assert_eq!(request.conversation_id.as_deref(), Some("conv-1"));
        assert!(matches!(
            request.action.and_then(|action| action.action),
            Some(conversation_action::Action::UserMessageAction(_))
        ));
        let state = request.conversation_state.expect("state");
        assert_eq!(state.root_prompt_messages_json.len(), 1);
        // senpi: the active user message rides in the action, so a context whose
        // only message is that user message builds no history turns.
        assert!(state.turns.is_empty());
        assert!(output.requested_model.parameters.is_empty());
    }

    #[test]
    fn builds_a_resume_action_when_the_prompt_forces_one() {
        let model = model("gpt-5.5-medium", None);
        let context = Context {
            system_prompt: None,
            messages: vec![Message::User(crate::types::UserMessage {
                content: crate::types::UserContent::Text("hello".to_owned()),
                timestamp: 0,
            })],
            tools: None,
        };
        let output = build_grpc_request(
            &model,
            &context,
            None,
            GrpcRequestState {
                conversation_id: "conv-1".to_owned(),
                blob_store: Arc::new(Mutex::new(ConversationBlobStore::new())),
                conversation_state: None,
                force_resume_action: true,
                pinned_requested_model: None,
                pinned_model_details: None,
            },
        )
        .expect("request");
        let message = AgentClientMessage::decode(output.request_bytes.as_slice()).expect("decode");
        let Some(agent_client_message::Message::RunRequest(request)) = message.message else {
            panic!("expected a run request");
        };
        assert!(matches!(
            request.action.and_then(|action| action.action),
            Some(conversation_action::Action::ResumeAction(_))
        ));
    }

    #[test]
    fn process_interaction_update_streams_text_thinking_and_usage() {
        let stream = AssistantMessageEventStream::assistant();
        let mut output = assistant_for_tests();
        let mut state = empty_block_state();
        let mut usage_state = UsageState::default();

        for message in [
            interaction_update::Message::ThinkingDelta(crate::api::cursor_agent::r#gen::agent_pb::ThinkingDeltaUpdate {
                text: "pondering".to_owned(),
            }),
            interaction_update::Message::TextDelta(crate::api::cursor_agent::r#gen::agent_pb::TextDeltaUpdate {
                text: "Hello ".to_owned(),
            }),
            interaction_update::Message::TextDelta(crate::api::cursor_agent::r#gen::agent_pb::TextDeltaUpdate {
                text: "world".to_owned(),
            }),
            interaction_update::Message::TokenDelta(crate::api::cursor_agent::r#gen::agent_pb::TokenDeltaUpdate { tokens: 7 }),
            interaction_update::Message::TurnEnded(Default::default()),
        ] {
            let update = crate::api::cursor_agent::r#gen::agent_pb::InteractionUpdate { message: Some(message) };
            process_interaction_update(&update, &mut output, &stream, &mut state, &mut usage_state);
        }

        assert_eq!(output.usage.output, 7);
        assert_eq!(output.stop_reason, crate::types::StopReason::Stop);
        let kinds: Vec<&str> = output.content.iter().map(ContentBlock::type_name).collect();
        assert_eq!(kinds, vec!["thinking", "text"]);
        match &output.content[1] {
            ContentBlock::Text(text) => assert_eq!(text.text, "Hello world"),
            other => panic!("expected text, got {other:?}"),
        }
        let event_kinds: Vec<&str> = stream
            .queue()
            .iter()
            .map(|event| match event {
                AssistantMessageEvent::ThinkingStart { .. } => "thinking_start",
                AssistantMessageEvent::ThinkingDelta { .. } => "thinking_delta",
                AssistantMessageEvent::TextStart { .. } => "text_start",
                AssistantMessageEvent::TextDelta { .. } => "text_delta",
                _ => "other",
            })
            .collect();
        assert_eq!(
            event_kinds,
            vec!["thinking_start", "thinking_delta", "text_start", "text_delta", "text_delta"]
        );
    }

    #[test]
    fn applies_billed_turn_ended_usage_with_a_cache_inclusive_input() {
        let mut output = assistant_for_tests();
        let mut usage_state = UsageState::default();
        let update = crate::api::cursor_agent::r#gen::agent_pb::TurnEndedUpdate {
            input_tokens: Some(17_989),
            output_tokens: Some(9),
            cache_read_tokens: Some(17_575),
            cache_write_tokens: Some(411),
            ..Default::default()
        };
        apply_billed_turn_ended_usage(&update, &mut output, &mut usage_state);
        assert_eq!(output.usage.input, 3);
        assert_eq!(output.usage.output, 9);
        assert_eq!(output.usage.cache_read, 17_575);
        assert_eq!(output.usage.cache_write, 411);
        assert_eq!(output.usage.total_tokens, 3 + 9 + 17_575 + 411);
    }

    #[test]
    fn keeps_the_delta_accumulated_output_when_turn_ended_omits_output_tokens() {
        let mut output = assistant_for_tests();
        output.usage.output = 7;
        let mut usage_state = UsageState::default();
        let update = crate::api::cursor_agent::r#gen::agent_pb::TurnEndedUpdate {
            input_tokens: Some(100),
            cache_read_tokens: Some(50),
            ..Default::default()
        };
        apply_billed_turn_ended_usage(&update, &mut output, &mut usage_state);
        assert_eq!(output.usage.input, 50);
        assert_eq!(output.usage.output, 7);
        assert_eq!(output.usage.cache_read, 50);
        assert_eq!(output.usage.total_tokens, 107);
    }

    #[test]
    fn ignores_billed_cache_read_that_dwarfs_checkpoint_used_tokens() {
        let mut output = assistant_for_tests();
        output.usage.output = 5;
        let mut usage_state = UsageState { live_used_tokens: Some(148_256), ..UsageState::default() };
        let update = crate::api::cursor_agent::r#gen::agent_pb::TurnEndedUpdate {
            input_tokens: Some(4_090_000),
            output_tokens: Some(5),
            cache_read_tokens: Some(3_990_000),
            cache_write_tokens: Some(100),
            ..Default::default()
        };
        apply_billed_turn_ended_usage(&update, &mut output, &mut usage_state);
        assert_eq!(output.usage.cache_read, 0);
        assert_eq!(output.usage.total_tokens, 148_256);
        assert!(output.usage.total_tokens < 200_000);
    }

    #[test]
    fn applies_checkpoint_used_tokens_to_in_flight_usage() {
        let mut output = assistant_for_tests();
        output.usage.output = 5;
        let mut usage_state = UsageState::default();
        let checkpoint = ConversationStateStructure {
            token_details: Some(crate::api::cursor_agent::r#gen::agent_pb::ConversationTokenDetails {
                used_tokens: 17_962,
                max_tokens: 200_000,
            }),
            ..Default::default()
        };
        apply_checkpoint_token_details(&checkpoint, &mut output, &mut usage_state);
        assert_eq!(output.usage.input, 17_957);
        assert_eq!(output.usage.output, 5);
        assert_eq!(output.usage.total_tokens, 17_962);
    }

    #[test]
    fn does_not_let_a_late_checkpoint_clobber_billed_turn_ended_usage() {
        let mut output = assistant_for_tests();
        let mut usage_state = UsageState::default();
        apply_billed_turn_ended_usage(
            &crate::api::cursor_agent::r#gen::agent_pb::TurnEndedUpdate {
                input_tokens: Some(17_579),
                output_tokens: Some(5),
                cache_read_tokens: Some(17_574),
                ..Default::default()
            },
            &mut output,
            &mut usage_state,
        );
        let checkpoint = ConversationStateStructure {
            token_details: Some(crate::api::cursor_agent::r#gen::agent_pb::ConversationTokenDetails {
                used_tokens: 999_999,
                max_tokens: 200_000,
            }),
            ..Default::default()
        };
        apply_checkpoint_token_details(&checkpoint, &mut output, &mut usage_state);
        assert_eq!(output.usage.input, 5);
        assert_eq!(output.usage.output, 5);
        assert_eq!(output.usage.cache_read, 17_574);
        assert_eq!(output.usage.total_tokens, 5 + 5 + 17_574);
    }

    #[test]
    fn flushes_server_owned_blocks_with_a_closing_toolcall_end() {
        let stream = AssistantMessageEventStream::assistant();
        let mut output = assistant_for_tests();
        let mut state = empty_block_state();
        state.current_tool_call = Some(0);
        output.content.push(ContentBlock::ToolCall(ToolCall {
            id: "scm-1".to_owned(),
            name: "connect_scm".to_owned(),
            arguments: Map::new(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        }));
        state.streaming.insert(
            0,
            StreamingBlockState {
                block_index: Some(0),
                block_kind: Some("connect-scm".to_owned()),
                ..StreamingBlockState::default()
            },
        );
        flush_open_tool_calls(&mut output, &stream, &mut state);
        assert!(state.open_tool_calls.is_empty());
        assert_eq!(state.current_tool_call, None);
        assert!(matches!(stream.queue().first(), Some(AssistantMessageEvent::ToolcallEnd { .. })));
    }
}
