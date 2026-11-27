//! `thread_create` parameter schema (`thread/contracts/create-params.ts`).

use serde_json::{Value, json};

use super::fields::{THREAD_MESSAGE_MAX_BYTES, thinking_level_schema, thread_delivery_mode_schema};
use super::permission_presets::PERMISSION_PRESETS;

pub fn thread_create_params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "thread": {
                "type": "string",
                "minLength": 1,
                "description": "Id or alias of an EXISTING thread to open, on whichever of the account's devices holds it; leave unset to create a new thread. Never combined with device."
            },
            "device": {
                "type": "string",
                "minLength": 1,
                "description": "Account device to create the new thread on (from thread_list with all_scope, which is tool.thread_list({ all_scope: true }) in an eval cell); leave unset to create it on this device. Only for a new thread."
            },
            "alias": {
                "type": "string",
                "minLength": 1,
                "maxLength": 64,
                "description": "Account-wide alias for the new thread, usable instead of its id in every thread tool; an alias another thread holds returns alias_taken naming the holder. With no signed-in account it becomes the thread's local name instead (unless name is set) and the result carries alias_scope \"local\"."
            },
            "name": {
                "type": "string",
                "description": "Display label for the new thread; an existing thread with the same normalized name returns name_conflict, and later renames change only the label while the thread value stays the address."
            },
            "cwd": {
                "type": "string",
                "description": "Working directory the new thread runs in; leaving it unset keeps the thread inside the caller's workspace."
            },
            "worktree": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "branch": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 200,
                        "description": "New branch the worktree checks out; must not exist yet. Default: omo/thread-<time>."
                    },
                    "base": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 200,
                        "description": "Commit, branch or tag the new branch starts at. Default: the repository's HEAD."
                    }
                },
                "description": "Create a new git worktree for the new thread and start the thread in it."
            },
            "fork_from": {
                "type": "string",
                "description": "Durable id of an existing thread to fork (refused unsupported until engine supports fork on open)."
            },
            "skills": {
                "type": "array",
                "items": { "type": "string", "minLength": 1, "maxLength": 64 },
                "minItems": 1,
                "maxItems": 16,
                "description": "Names of installed skills the new thread follows from its first turn."
            },
            "model": {
                "type": "string",
                "minLength": 3,
                "description": "Model the new thread runs on, as an exact provider/id."
            },
            "reasoning": thinking_level_schema("Thinking level the new thread starts with."),
            "permission_preset": {
                "type": "string",
                "enum": PERMISSION_PRESETS,
                "description": "Approval mode the new thread runs in: this session's own mode or one that allows less."
            },
            "prompt": {
                "type": "string",
                "minLength": 1,
                "maxLength": THREAD_MESSAGE_MAX_BYTES,
                "description": "First message for the new thread, delivered once the thread exists."
            },
            "delivery": thread_delivery_mode_schema(),
            "idempotency_key": { "type": "string" }
        }
    })
}
