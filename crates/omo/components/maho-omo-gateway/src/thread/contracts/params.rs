//! Thread tool parameter structs & JSON schemas (`thread/contracts/params.ts`).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::super::worktree::WorktreeRequest;
use super::create_params::thread_create_params_schema;
use super::fields::{
    THREAD_MESSAGE_MAX_BYTES, THREAD_READ_MAX_BYTES, THREAD_SUMMARY_MAX_LENGTH,
    thinking_level_schema, thread_delivery_mode_schema,
};
use super::state::ThreadState;

pub const THREAD_READ_WAIT_MAX_MS: u64 = 600_000;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadCreateInput {
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub worktree: Option<WorktreeRequest>,
    #[serde(default)]
    pub fork_from: Option<String>,
    #[serde(default)]
    pub skills: Option<Vec<String>>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub reasoning: Option<String>,
    #[serde(default)]
    pub permission_preset: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub delivery: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadListInput {
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub state: Option<Vec<ThreadState>>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadReadInput {
    pub thread: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub max_bytes: Option<usize>,
    #[serde(default)]
    pub wait_ms: Option<u64>,
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadSendInput {
    pub thread: String,
    pub message: String,
    #[serde(default)]
    pub delivery: Option<String>,
    #[serde(default)]
    pub expected_turn_id: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub collapse_key: Option<String>,
    #[serde(default)]
    pub all_scope: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadInterruptInput {
    pub thread: String,
    #[serde(default)]
    pub turn_id: Option<String>,
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadCloseInput {
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub force: Option<bool>,
    #[serde(default)]
    pub graceful: Option<bool>,
    #[serde(default)]
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadHandoffInput {
    pub thread: String,
    #[serde(default, rename = "match")]
    pub match_mode: Option<String>,
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub delivery: Option<String>,
    #[serde(default)]
    pub expected_turn_id: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
    #[serde(default)]
    pub all_scope: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadRenameInput {
    #[serde(default)]
    pub thread: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub alias: Option<String>,
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadSetModelInput {
    pub thread: String,
    pub model: String,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadSetReasoningInput {
    pub thread: String,
    pub level: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ThreadArchiveInput {
    pub thread: String,
    #[serde(default)]
    pub archived: Option<bool>,
    #[serde(default)]
    pub all_scope: Option<bool>,
    #[serde(default)]
    pub idempotency_key: Option<String>,
}

pub fn thread_tool_param_schema(name: &str) -> Option<Value> {
    Some(match name {
        "thread_create" => thread_create_params_schema(),
        "thread_list" => json!({
            "type": "object",
            "properties": {
                "all_scope": { "type": "boolean" },
                "state": { "type": "array", "items": { "type": "string", "enum": ["running", "idle", "inactive", "archived"] }, "minItems": 1 },
                "idempotency_key": { "type": "string" }
            }
        }),
        "thread_read" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string" },
                "cursor": { "type": "string" },
                "max_bytes": { "type": "integer", "minimum": 1, "maximum": THREAD_READ_MAX_BYTES },
                "wait_ms": { "type": "integer", "minimum": 1, "maximum": THREAD_READ_WAIT_MAX_MS },
                "all_scope": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["thread"]
        }),
        "thread_send" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string" },
                "message": { "type": "string", "maxLength": THREAD_MESSAGE_MAX_BYTES },
                "delivery": thread_delivery_mode_schema(),
                "expected_turn_id": { "type": "string" },
                "summary": { "type": "string", "maxLength": THREAD_SUMMARY_MAX_LENGTH },
                "idempotency_key": { "type": "string" },
                "collapse_key": { "type": "string", "minLength": 1, "maxLength": 200 },
                "all_scope": { "type": "boolean" }
            },
            "required": ["thread", "message"]
        }),
        "thread_interrupt" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string" },
                "turn_id": { "type": "string" },
                "all_scope": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["thread"]
        }),
        "thread_close" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string", "minLength": 1 },
                "idempotency_key": { "type": "string" },
                "all_scope": { "type": "boolean" },
                "force": { "type": "boolean" },
                "graceful": { "type": "boolean" },
                "wait_ms": { "type": "integer", "minimum": 1, "maximum": 600_000 }
            }
        }),
        "thread_handoff" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string" },
                "match": { "type": "string", "enum": ["exact", "fuzzy"] },
                "message": { "type": "string", "maxLength": THREAD_MESSAGE_MAX_BYTES },
                "delivery": thread_delivery_mode_schema(),
                "expected_turn_id": { "type": "string" },
                "summary": { "type": "string", "maxLength": THREAD_SUMMARY_MAX_LENGTH },
                "idempotency_key": { "type": "string" },
                "all_scope": { "type": "boolean" }
            },
            "required": ["thread", "message"]
        }),
        "thread_rename" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string", "minLength": 1 },
                "name": { "type": "string", "minLength": 1, "maxLength": 200 },
                "alias": { "type": "string", "minLength": 1, "maxLength": 64 },
                "all_scope": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            }
        }),
        "thread_set_model" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string" },
                "model": { "type": "string", "minLength": 1 },
                "provider": { "type": "string" },
                "all_scope": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["thread", "model"]
        }),
        "thread_set_reasoning" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string" },
                "level": thinking_level_schema("Thinking level to apply."),
                "scope": { "type": "string", "enum": ["session", "turn"] },
                "all_scope": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["thread", "level"]
        }),
        "thread_archive" => json!({
            "type": "object",
            "properties": {
                "thread": { "type": "string" },
                "archived": { "type": "boolean" },
                "all_scope": { "type": "boolean" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["thread"]
        }),
        "thread_report" => json!({
            "type": "object",
            "properties": {
                "binding_id": { "type": "string", "minLength": 1 },
                "kind": { "type": "string", "enum": ["milestone", "report", "question", "completion"] },
                "text": { "type": "string" },
                "request_id": { "type": "string", "minLength": 1 },
                "request_kind": { "type": "string", "enum": ["question", "select", "confirm", "input", "editor"] },
                "idempotency_key": { "type": "string" }
            },
            "required": ["kind", "text"]
        }),
        "thread_outbox" => json!({
            "type": "object",
            "properties": {
                "binding_id": { "type": "string", "minLength": 1 },
                "after_cursor": { "type": "integer", "minimum": 0 },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500 },
                "idempotency_key": { "type": "string" }
            },
            "required": ["binding_id"]
        }),
        "thread_outbox_ack" => json!({
            "type": "object",
            "properties": {
                "binding_id": { "type": "string", "minLength": 1 },
                "cursor": { "type": "integer", "minimum": 1 },
                "provider_message_id": { "type": "string", "minLength": 1, "maxLength": 256 },
                "idempotency_key": { "type": "string" }
            },
            "required": ["binding_id", "cursor"]
        }),
        "thread_answer" => json!({
            "type": "object",
            "properties": {
                "binding_id": { "type": "string", "minLength": 1 },
                "reply_token": { "type": "string", "minLength": 1 },
                "answer": { "type": "string" },
                "idempotency_key": { "type": "string" }
            },
            "required": ["binding_id", "reply_token", "answer"]
        }),
        _ => return None,
    })
}
