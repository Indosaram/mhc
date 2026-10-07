//! `tools/task/params.ts`: the task tool's parameter schema (TypeBox object modelled as JSON Schema).

use std::sync::LazyLock;

use serde_json::{Value, json};

use crate::task_summary::TASK_SUMMARY_MAX_LENGTH;

pub const MAX_TASK_BATCH_ITEMS: usize = 16;

fn build_task_tool_params() -> Value {
    json!({
        "type": "object",
        "properties": {
            "prompt": {
                "type": "string",
                "description": "The instruction for the child task. MUST be written in English. Mutually exclusive with tasks; provide exactly one of prompt or tasks."
            },
            "task_summary": {
                "type": "string",
                "maxLength": TASK_SUMMARY_MAX_LENGTH,
                "description": "One-line summary of the delegated work, shown to the user in the task footer/widget UI instead of the raw prompt. Keep it within 80 chars; longer values are force-truncated."
            },
            "description": {
                "type": "string",
                "description": "Short human label for this task, shown in status views."
            },
            "category": {
                "type": "string",
                "description": "Category name to route through Sisyphus-Junior. Mutually exclusive with subagent_type; required unless subagent_type is given."
            },
            "subagent_type": {
                "type": "string",
                "description": "Agent name to invoke directly (e.g. momus). Mutually exclusive with category; required unless category is given."
            },
            "run_in_background": {
                "type": "boolean",
                "description": "true returns a child task id immediately; false (default) waits and returns the final response."
            },
            "isolated": {
                "type": "boolean",
                "description": "Run the child in a copy-on-write clone of the checkout and merge its changes back on completion; defaults to task.isolation.enabled."
            },
            "apply": {
                "type": "boolean",
                "description": "Merge the child's changes into this checkout when it completes; false keeps the patch/branch artifacts only."
            },
            "merge": {
                "type": "string",
                "enum": ["patch", "branch"],
                "description": "Merge strategy for an isolated child; defaults to task.isolation.merge."
            },
            "name": {
                "type": "string",
                "description": "Optional stable name for this task within the current session; must be unique within the session."
            },
            "model": {
                "type": "string",
                "description": "Explicit model override, e.g. anthropic/claude-opus-4. Only valid with subagent_type; mutually exclusive with category — category-routed tasks take their model from omo.json (categories.<name>.models)."
            },
            "load_skills": {
                "type": "array",
                "items": { "type": "string" },
                "description": "Skill names whose SKILL.md content is prepended to the child prompt. Defaults to []."
            },
            "tasks": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "description": "The instruction for this child task. MUST be written in English."
                        },
                        "task_summary": {
                            "type": "string",
                            "maxLength": TASK_SUMMARY_MAX_LENGTH,
                            "description": "One-line summary of this task's delegated work, shown in the task footer/widget UI. Longer values are force-truncated to 80 chars."
                        },
                        "description": {
                            "type": "string",
                            "description": "Short human label for this task."
                        },
                        "isolated": {
                            "type": "boolean",
                            "description": "Run the child in a copy-on-write clone of the checkout and merge its changes back on completion; defaults to task.isolation.enabled."
                        },
                        "apply": {
                            "type": "boolean",
                            "description": "Merge the child's changes into this checkout when it completes; false keeps the patch/branch artifacts only."
                        },
                        "merge": {
                            "type": "string",
                            "enum": ["patch", "branch"],
                            "description": "Merge strategy for an isolated child; defaults to task.isolation.merge."
                        },
                        "category": {
                            "type": "string",
                            "description": "Category name for this task."
                        },
                        "subagent_type": {
                            "type": "string",
                            "description": "Direct agent name for this task."
                        },
                        "name": {
                            "type": "string",
                            "description": "Optional stable name for this task."
                        },
                        "model": {
                            "type": "string",
                            "description": "Model override for this task. Only valid when the item's effective target is subagent_type; rejected with a category target."
                        },
                        "load_skills": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Skills loaded for this task."
                        }
                    },
                    "required": ["prompt"]
                },
                "maxItems": MAX_TASK_BATCH_ITEMS,
                "description": "Batch of up to 16 child tasks to spawn in one call. Empty provider padding is normalized before validation. Mutually exclusive with prompt; top-level category/subagent_type/model/load_skills are inherited by items that omit them. An item whose effective target is a category must not carry a model (own or inherited)."
            }
        }
    })
}

/// The task tool parameter schema. No top-level field is schema-required: the prompt-XOR-tasks
/// rule is enforced by batch-shape validation.
pub static TASK_TOOL_PARAMS: LazyLock<Value> = LazyLock::new(build_task_tool_params);
