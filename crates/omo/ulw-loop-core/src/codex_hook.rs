//! Codex lifecycle hook integration.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::paths::UlwLoopScope;
use crate::steering::{parse_ulw_loop_steering_directive, steer_ulw_loop};
use crate::ultrawork_directive::{build_ultrawork_directive_output, UltraworkDirectiveInput};
use crate::ultrawork_skill_pointer::UltraworkAdditionalContextOptions;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct UserPromptSubmitPayload {
    pub cwd: String,
    pub hook_event_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    pub prompt: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct UserPromptSubmitHookOptions {
    pub include_ultrawork_directive: bool,
    pub ultrawork_skill_file_path: Option<Option<PathBuf>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreToolUsePayload {
    pub cwd: String,
    pub hook_event_name: String,
    pub model: String,
    pub permission_mode: String,
    pub session_id: String,
    pub tool_input: Value,
    pub tool_name: String,
    pub tool_use_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub turn_id: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreToolUseHookOutput {
    #[serde(rename = "hookSpecificOutput")]
    pub hook_specific_output: PreToolUseSpecificOutput,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreToolUseSpecificOutput {
    #[serde(rename = "hookEventName")]
    pub hook_event_name: String,
    #[serde(rename = "permissionDecision")]
    pub permission_decision: String,
    #[serde(rename = "permissionDecisionReason")]
    pub permission_decision_reason: String,
    #[serde(rename = "additionalContext")]
    pub additional_context: String,
}

const CREATE_GOAL_TOOL_NAME: &str = "create_goal";
const CREATE_GOAL_PAYLOAD_WARNING: &str =
    "Use create_goal with objective only. Omit token_budget so the goal stays unlimited, and put lifecycle status changes on update_goal.";

pub fn parse_user_prompt_submit_payload(raw: &str) -> Option<UserPromptSubmitPayload> {
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str(raw).ok()
}

pub fn parse_pre_tool_use_payload(raw: &str) -> Option<PreToolUsePayload> {
    if raw.trim().is_empty() {
        return None;
    }
    serde_json::from_str(raw).ok()
}

fn has_steering_directive_marker(prompt: &str) -> bool {
    // PERF-DIVERGENCE(omo codex-hook): upstream builds this RegExp per prompt; this port compiles it once in a LazyLock. Output is identical.
    static RE_MARKER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:^|\s)(?:OMO_ULW_LOOP_STEER|omo\.ulw-loop\.steer|omo ulw-loop steer|ulw-loop steer):").expect("valid steering regex"));
    RE_MARKER.is_match(prompt)
}

pub fn apply_user_prompt_ulw_loop_steering(
    payload: &UserPromptSubmitPayload,
    options: &UserPromptSubmitHookOptions,
) -> String {
    if payload.hook_event_name != "UserPromptSubmit" {
        return String::new();
    }
    let proposal = parse_ulw_loop_steering_directive(&payload.prompt);
    if proposal.is_none() {
        if has_steering_directive_marker(&payload.prompt) {
            return String::new();
        }
        if !options.include_ultrawork_directive {
            return String::new();
        }
        let transcript = payload.transcript_path.as_deref().map(Path::new);
        let input = UltraworkDirectiveInput {
            prompt: &payload.prompt,
            transcript_path: transcript,
        };
        let ctx_opts = UltraworkAdditionalContextOptions {
            skill_file_path: options.ultrawork_skill_file_path.clone(),
        };
        if options.ultrawork_skill_file_path.is_none() {
            return crate::ultrawork_skill_pointer::build_ultrawork_additional_context(&ctx_opts);
        }
        return build_ultrawork_directive_output(&input, &ctx_opts);
    }

    let Some(proposal) = proposal else { return String::new(); };
    let scope = UlwLoopScope {
        session_id: Some(payload.session_id.clone()),
        ..Default::default()
    };

    let Ok(result) = steer_ulw_loop(&payload.cwd, &proposal, Some(&scope), None) else {
        return String::new();
    };

    if !result.accepted {
        return String::new();
    }

    let out = serde_json::json!({
        "status": "accepted",
        "kind": result.audit.kind,
        "source": result.audit.source,
        "deduped": result.deduped,
    });
    serde_json::to_string(&out).unwrap_or_default()
}

pub fn apply_pre_tool_use_goal_budget_guard(payload: &PreToolUsePayload) -> String {
    if payload.hook_event_name != "PreToolUse" {
        return String::new();
    }
    if payload.tool_name != CREATE_GOAL_TOOL_NAME {
        return String::new();
    }
    if !has_invalid_create_goal_input(&payload.tool_input) {
        return String::new();
    }

    let output = PreToolUseHookOutput {
        hook_specific_output: PreToolUseSpecificOutput {
            hook_event_name: "PreToolUse".to_string(),
            permission_decision: "deny".to_string(),
            permission_decision_reason: CREATE_GOAL_PAYLOAD_WARNING.to_string(),
            additional_context: CREATE_GOAL_PAYLOAD_WARNING.to_string(),
        },
    };

    let mut json = serde_json::to_string(&output).unwrap_or_default();
    json.push('\n');
    json
}

fn has_invalid_create_goal_input(val: &Value) -> bool {
    let Some(obj) = val.as_object() else {
        return false;
    };
    obj.keys().any(|k| k != "objective")
}

pub fn run_ulw_loop_hook_cli<R: Read, W: Write>(
    mut stdin: R,
    mut stdout: W,
    options: &UserPromptSubmitHookOptions,
) {
    let mut raw = String::new();
    if stdin.read_to_string(&mut raw).is_err() {
        return;
    }
    let Some(payload) = parse_user_prompt_submit_payload(&raw) else {
        return;
    };
    let output = apply_user_prompt_ulw_loop_steering(&payload, options);
    if !output.is_empty() {
        let _ = stdout.write_all(output.as_bytes());
    }
}

pub fn run_pre_tool_use_goal_budget_guard_cli<R: Read, W: Write>(mut stdin: R, mut stdout: W) {
    let mut raw = String::new();
    if stdin.read_to_string(&mut raw).is_err() {
        return;
    }
    let Some(payload) = parse_pre_tool_use_payload(&raw) else {
        return;
    };
    let output = apply_pre_tool_use_goal_budget_guard(&payload);
    if !output.is_empty() {
        let _ = stdout.write_all(output.as_bytes());
    }
}
