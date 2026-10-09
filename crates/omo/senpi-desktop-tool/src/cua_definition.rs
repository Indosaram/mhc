//! OpenAI CUA tool definition matching upstream `src/cua-definition.ts`.

use std::sync::Arc;
use maho_tools::definition::{
    ToolDefinition, ToolExecutionMode, ToolExposure, ToolPermissionRequest,
};
use serde_json::json;

use crate::cua_actions::is_read_only_actions;
use crate::permission::COMPUTER_PERMISSION;

pub const COMPUTER_ACTIONS_TOOL_NAME: &str = "computer_actions";

pub const CUA_DESCRIPTION: &[&str] = &[
    "Drive the user's real desktop with OpenAI computer-use actions (experimental): screenshot, click, double_click, move, drag, scroll, type, keypress, wait, or a batch of them.",
    "x,y are pixels of the latest screenshot. A batch stops at the first failed action; an in-batch screenshot becomes the frame for the actions after it.",
    "Pass only the fields the chosen action takes (each field's description names its actions); anything else is refused with COMPUTER_INVALID_ARGUMENTS before any input.",
    "Errors carry COMPUTER_* codes with a recovery hint. COMPUTER_SUSPENDED or COMPUTER_SUPERVISOR_NOT_LIVE means the user stopped you: stop and wait.",
];

pub fn computer_actions_params_schema() -> serde_json::Value {
    let action_names = json!([
        "screenshot", "click", "double_click", "move", "drag", "scroll", "type", "keypress", "wait", "batch"
    ]);

    json!({
        "type": "object",
        "properties": {
            "action": {
                "type": "string",
                "enum": action_names,
                "description": "The action to perform, or batch to run actions in order"
            },
            "actions": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "action": {
                            "type": "string",
                            "enum": ["screenshot", "click", "double_click", "move", "drag", "scroll", "type", "keypress", "wait"],
                            "description": "The action to perform"
                        },
                        "x": { "type": "number", "description": "click, double_click, move, drag (start), scroll (required): x pixel" },
                        "y": { "type": "number", "description": "click, double_click, move, drag (start), scroll (required): y pixel" },
                        "button": { "type": "string", "enum": ["left", "right", "middle"], "description": "click, double_click, move, drag: mouse button (default left)" },
                        "to_x": { "type": "number", "description": "drag (required): end x pixel" },
                        "to_y": { "type": "number", "description": "drag (required): end y pixel" },
                        "scroll_x": { "type": "number", "description": "scroll (required): horizontal scroll distance in pixels on every OS (one wheel notch is about 40 px); positive scrolls right" },
                        "scroll_y": { "type": "number", "description": "scroll (required): vertical scroll distance in pixels on every OS (one wheel notch is about 40 px); positive moves the view toward the end of the content (down)" },
                        "text": { "type": "string", "description": "type (required): text to type" },
                        "keys": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "keypress (required): keys pressed together" },
                        "ms": { "type": "integer", "minimum": 0, "description": "wait (required): milliseconds" },
                        "timeout": { "type": "number", "exclusiveMinimum": 0, "description": "Maximum time in seconds for this action." },
                        "include_screenshot": { "type": "boolean", "description": "Capture a screenshot after the action." }
                    },
                    "additionalProperties": false
                },
                "minItems": 1,
                "description": "batch (required): actions to execute in order"
            },
            "x": { "type": "number", "description": "click, double_click, move, drag (start), scroll (required): x pixel" },
            "y": { "type": "number", "description": "click, double_click, move, drag (start), scroll (required): y pixel" },
            "button": { "type": "string", "enum": ["left", "right", "middle"], "description": "click, double_click, move, drag: mouse button (default left)" },
            "to_x": { "type": "number", "description": "drag (required): end x pixel" },
            "to_y": { "type": "number", "description": "drag (required): end y pixel" },
            "scroll_x": { "type": "number", "description": "scroll (required): horizontal scroll distance in pixels on every OS (one wheel notch is about 40 px); positive scrolls right" },
            "scroll_y": { "type": "number", "description": "scroll (required): vertical scroll distance in pixels on every OS (one wheel notch is about 40 px); positive moves the view toward the end of the content (down)" },
            "text": { "type": "string", "description": "type (required): text to type" },
            "keys": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "keypress (required): keys pressed together" },
            "ms": { "type": "integer", "minimum": 0, "description": "wait (required): milliseconds" },
            "timeout": { "type": "number", "exclusiveMinimum": 0, "description": "Maximum time in seconds for this action." },
            "include_screenshot": { "type": "boolean", "description": "Capture a screenshot after the action." }
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

pub fn computer_actions_permission_parser(
    _tool_name: &str,
    input: &serde_json::Value,
    _cwd: &std::path::Path,
) -> Vec<ToolPermissionRequest> {
    let tier = if is_read_only_actions(input) { "read" } else { "exec" };
    vec![ToolPermissionRequest::new(
        COMPUTER_PERMISSION,
        vec![tier.to_string()],
        vec![tier.to_string()],
    )]
}

pub fn computer_actions_tool_definition() -> ToolDefinition {
    let execute_stub: maho_tools::definition::ToolExecutor = Arc::new(|_| Box::pin(async { Err(maho_tools::ToolError::Message("computer_actions implementation is not bound".into())) }));
    let mut def = ToolDefinition::new(
        COMPUTER_ACTIONS_TOOL_NAME,
        &CUA_DESCRIPTION.join("
"),
        computer_actions_params_schema(),
        execute_stub,
    );
    def.label = "Computer actions".to_string();
    def.exposure = Some(ToolExposure::Search);
    def.search_text = Some("OpenAI computer-use actions (experimental): screenshot, click, type, keypress, scroll, drag, batch".to_string());
    def.search_keywords = Some(vec![
        "computer use".to_string(),
        "cua".to_string(),
        "openai computer".to_string(),
        "click".to_string(),
        "screenshot".to_string(),
        "keypress".to_string(),
    ]);
    def.search_group = Some("desktop".to_string());
    def.execution_mode = Some(ToolExecutionMode::Sequential);
    def
}
