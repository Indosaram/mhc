//! Spawn guards enforcement for subagents.

use std::collections::BTreeSet;
use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use regex::Regex;
use serde_json::Value;

use crate::codex_hook::{parse_pre_tool_use_payload, PreToolUsePayload};
use crate::goal_status::is_final_run_completion_candidate;
use crate::paths::{ulw_loop_attempt_evidence_dir, ulw_loop_dir, ulw_loop_state_lock_path, UlwLoopScope};
use crate::plan_io::read_ulw_loop_plan;
use crate::registered_agent_roles::registered_agent_roles;
use crate::spawn_budget_io::{atomic_write_json, is_non_empty_file, read_admission_breaker, read_admission_breaker_with_env, read_count, read_counts};
use crate::spawn_role_guard::spawn_role_denial;
use crate::state_lock::with_state_lock;
use crate::surface::{canonical_reviewer_agent_name, resolve_toolkit_surface, reviewer_roles_for, GATE_REVIEWER_AGENT_NAMES, UlwLoopToolkitSurface};

pub const DEFAULT_FANOUT_LIMIT: u32 = 24;
const DEFAULT_REVIEW_SPAWN_LIMIT: u32 = 3;

fn is_spawn_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "spawn_agent" | "multi_agent_v1.spawn_agent" | "collaborationspawn_agent" | "collaboration.spawn_agent"
    )
}

pub struct SpawnGuardOptions {
    pub lock_timeout_ms: Option<u64>,
}

pub fn apply_spawn_guards(payload: &PreToolUsePayload, options: Option<&SpawnGuardOptions>) -> String {
    apply_spawn_guards_with_env(payload, options, |key| env::var(key).ok())
}

pub fn apply_spawn_guards_with_env(
    payload: &PreToolUsePayload,
    options: Option<&SpawnGuardOptions>,
    get_env: impl Fn(&str) -> Option<String> + Copy,
) -> String {
    if payload.hook_event_name != "PreToolUse" || !is_spawn_tool(&payload.tool_name) {
        return String::new();
    }
    if crate::surface::resolve_toolkit_surface_with_env(get_env) == UlwLoopToolkitSurface::LazyCodex {
        let reason = spawn_role_denial(&payload.tool_input, || registered_agent_roles(Path::new(&payload.cwd), None));
        if let Some(r) = reason { return deny(&r); }
    }
    apply_spawn_budget_guards_with_env(payload, options, get_env)
}

pub fn apply_spawn_budget_guards(payload: &PreToolUsePayload, options: Option<&SpawnGuardOptions>) -> String {
    apply_spawn_budget_guards_with_env(payload, options, |key| env::var(key).ok())
}

pub fn apply_spawn_budget_guards_with_env(
    payload: &PreToolUsePayload,
    options: Option<&SpawnGuardOptions>,
    get_env: impl Fn(&str) -> Option<String> + Copy,
) -> String {
    if payload.hook_event_name != "PreToolUse" || !is_spawn_tool(&payload.tool_name) {
        return String::new();
    }
    if let Some(breaker) = read_admission_breaker_with_env(&payload.session_id, Some(Path::new(&payload.cwd)), get_env) {
        return deny(&format!(
            "Subagent admission failed earlier in this session ({breaker}). Do not spawn more workers or reviewers; report the capacity block and wait for the user."
        ));
    }

    let scope = UlwLoopScope {
        session_id: Some(payload.session_id.clone()),
        ..Default::default()
    };
    let state_dir = ulw_loop_dir(&payload.cwd, Some(&scope));
    let Ok(plan) = read_ulw_loop_plan(&payload.cwd, Some(&scope)) else {
        return String::new();
    };

    let lock_path = ulw_loop_state_lock_path(&payload.cwd, Some(&scope));
    let lock_opts = options.map(|o| crate::state_lock::StateLockOptions {
        timeout_ms: o.lock_timeout_ms,
        ..Default::default()
    });
    match with_state_lock(&lock_path, lock_opts.as_ref(), |_| {
        Ok(evaluate_guards(payload, &plan, &state_dir, get_env))
    }) {
        Ok(output) => output,
        Err(err) => deny(&format!("ulw-loop spawn guard could not take the session state lock: {err}")),
    }
}

fn evaluate_guards(payload: &PreToolUsePayload, plan: &crate::domain_types::UlwLoopPlan, state_dir: &Path, get_env: impl Fn(&str) -> Option<String> + Copy) -> String {
    if let Some(reason) = peek_fan_out_budget(state_dir, get_env) {
        return deny(&reason);
    }
    if let Some(missing) = missing_gate_artifact(payload, plan, get_env) {
        return deny(&format!("record manual QA first; gate audits its artifacts: missing {missing}"));
    }
    if let Some(reason) = consume_review_spawn_budget(payload, plan, state_dir, get_env) {
        return deny(&reason);
    }
    if let Some(reason) = consume_fan_out_budget(state_dir, get_env) {
        return deny(&reason);
    }
    String::new()
}

pub fn run_spawn_admission_recorder_cli<R: Read, W: Write>(stdin: R, stdout: W) {
    run_spawn_admission_recorder_cli_with_env(stdin, stdout, |key| env::var(key).ok());
}

pub fn run_spawn_admission_recorder_cli_with_env<R: Read, W: Write>(mut stdin: R, mut _stdout: W, get_env: impl Fn(&str) -> Option<String>) {
    let mut raw = String::new();
    if stdin.read_to_string(&mut raw).is_err() {
        return;
    }
    let Ok(payload) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    let response = payload
        .get("tool_response")
        .and_then(|v| v.as_str().map(|s| s.to_string()).or_else(|| Some(v.to_string())))
        .unwrap_or_default();

    // PERF-DIVERGENCE(omo spawn-guard): upstream builds this RegExp on every invocation; this port compiles it once in a LazyLock. Output is identical.
    static RE_LIMIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)too many active cells|AgentLimitReached|max_threads|max_concurrent_threads_per_session").expect("valid regex"));
    if !RE_LIMIT.is_match(&response) {
        return;
    }

    let Some(data_dir) = get_env("PLUGIN_DATA") else {
        return;
    };
    let Some(session_id) = payload.get("session_id").and_then(|v| v.as_str()) else {
        return;
    };

    let marker_dir = Path::new(&data_dir).join("spawn-breaker");
    let _ = fs::create_dir_all(&marker_dir);
    let marker_path = marker_dir.join(format!("{session_id}.json"));
    let now = crate::runtime::iso();
    let _ = atomic_write_json(
        &marker_path,
        &serde_json::json!({
            "reason": response,
            "at": now,
        }),
    );
}

pub fn run_spawn_guard_cli<R: Read, W: Write>(mut stdin: R, mut stdout: W) {
    let mut raw = String::new();
    if stdin.read_to_string(&mut raw).is_err() {
        return;
    }
    let Some(payload) = parse_pre_tool_use_payload(&raw) else {
        let _ = stdout.write_all(deny("LazyCodex spawn guard received an invalid hook payload; role routing was not verified.").as_bytes());
        return;
    };
    let output = apply_spawn_guards(&payload, None);
    if !output.is_empty() {
        let _ = stdout.write_all(output.as_bytes());
    }
}

fn peek_fan_out_budget(state_dir: &Path, get_env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let counter_path = state_dir.join("spawn-count.json");
    let count = read_count(&counter_path) + 1;
    let limit = fan_out_limit(get_env);
    if count <= limit {
        None
    } else {
        Some(format!(
            "ulw-loop spawn fan-out cap reached ({count}/{limit}). Consolidate work into the agents already running, or raise OMO_SPAWN_FANOUT_LIMIT if this volume is intentional."
        ))
    }
}

fn consume_fan_out_budget(state_dir: &Path, get_env: impl Fn(&str) -> Option<String>) -> Option<String> {
    let counter_path = state_dir.join("spawn-count.json");
    let count = read_count(&counter_path) + 1;
    let _ = atomic_write_json(&counter_path, &serde_json::json!({ "count": count }));
    let limit = fan_out_limit(get_env);
    if count <= limit {
        None
    } else {
        Some(format!(
            "ulw-loop spawn fan-out cap reached ({count}/{limit}). Consolidate work into the agents already running, or raise OMO_SPAWN_FANOUT_LIMIT if this volume is intentional."
        ))
    }
}

fn consume_review_spawn_budget(
    payload: &PreToolUsePayload,
    plan: &crate::domain_types::UlwLoopPlan,
    state_dir: &Path,
    get_env: impl Fn(&str) -> Option<String> + Copy,
) -> Option<String> {
    let agent_type = review_agent_type(&payload.tool_input, get_env)?;
    let goal = plan
        .goals
        .iter()
        .find(|g| Some(&g.id) == plan.active_goal_id.as_ref())
        .or_else(|| plan.goals.iter().find(|g| is_final_run_completion_candidate(plan, g)))?;

    let counter_path = state_dir.join("review-spawn-counts.json");
    let limit = review_spawn_limit(get_env);
    let mut counts = read_counts(&counter_path);
    let key = format!("{}:{}:a{}", agent_type, goal.id, goal.attempt);
    let count = counts.get(&key).copied().unwrap_or(0) + 1;
    if count > limit {
        return Some(format!(
            "ulw-loop reviewer no-progress cap reached ({agent_type} {count}/{limit}) for {} attempt {}. Consolidate existing review findings, or checkpoint and start a new attempt after concrete progress.",
            goal.id, goal.attempt
        ));
    }
    counts.insert(key, count);
    let _ = atomic_write_json(&counter_path, &counts);
    None
}

fn missing_gate_artifact(payload: &PreToolUsePayload, plan: &crate::domain_types::UlwLoopPlan, get_env: impl Fn(&str) -> Option<String> + Copy) -> Option<String> {
    if !is_gate_reviewer_spawn(&payload.tool_input, get_env) {
        return None;
    }
    let goal = plan.goals.iter().find(|g| is_final_run_completion_candidate(plan, g))?;
    if goal.status == "complete" {
        return None;
    }
    if !goal.success_criteria.iter().all(|c| c.status == "pass") {
        return None;
    }

    let scope = UlwLoopScope {
        session_id: Some(payload.session_id.clone()),
        ..Default::default()
    };
    let required_artifacts = vec![format!("{}-manual-qa.md", goal.id)];

    if plan.evidence_layout_version == Some(2) {
        let attempt_dir = ulw_loop_attempt_evidence_dir(&goal.id, goal.attempt, Some(&scope)).ok()?;
        for name in &required_artifacts {
            let relative = format!("{attempt_dir}/{name}");
            if !is_non_empty_file(&Path::new(&payload.cwd).join(&relative)) {
                let legacy = format!(".omo/evidence/{}", name);
                if !is_non_empty_file(&Path::new(&payload.cwd).join(&legacy)) {
                    return Some(relative);
                }
            }
        }
        return None;
    }

    let manual_qa = format!(".omo/evidence/{}-manual-qa.md", goal.id);
    if is_non_empty_file(&Path::new(&payload.cwd).join(&manual_qa)) {
        None
    } else {
        Some(manual_qa)
    }
}

fn is_gate_reviewer_spawn(tool_input: &Value, get_env: impl Fn(&str) -> Option<String> + Copy) -> bool {
    let Some(agent_type) = review_agent_type(tool_input, get_env) else {
        return false;
    };
    GATE_REVIEWER_AGENT_NAMES.contains(&agent_type.as_str())
}

// PERF-DIVERGENCE(omo spawn-guard): upstream rebuilds the reviewer role list and per-name regexes per lookup; this port builds the list and one alternation regex once. Matching results are identical.
static ALL_ROLES: LazyLock<Vec<&'static str>> = LazyLock::new(|| {
    let mut all_roles = Vec::new();
    for roles in crate::surface::all_reviewer_roles() {
        all_roles.push(roles.gate_review);
        all_roles.push(roles.code_review);
        all_roles.push(roles.manual_qa);
    }
    for (alias, _) in crate::surface::LEGACY_REVIEWER_AGENT_ALIASES {
        all_roles.push(alias);
    }
    all_roles
});

// PERF-DIVERGENCE(omo spawn-guard): the leftmost match of one alternation equals the earliest per-name match (role names are distinct and none is a prefix of another), so the per-name regex loop is replaced by one cached regex.
static RE_EXPLICIT_ROLE: LazyLock<Regex> = LazyLock::new(|| {
    let alternation = ALL_ROLES.iter().map(|name| regex::escape(name)).collect::<Vec<_>>().join("|");
    Regex::new(&format!(r"\bact as (?:an? )?({alternation})\b")).expect("valid regex")
});

// PERF-DIVERGENCE(omo spawn-guard): upstream recompiles this regex per call; this port compiles it once. Output is identical.
static RE_ACT_AS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\bact as (?:an? )?\S+").expect("valid regex"));

// PERF-DIVERGENCE(omo spawn-guard): upstream recompiles this regex per call; this port compiles it once. Output is identical.
static RE_GATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)lazycodex-gate-reviewer|omo-native-gate-reviewer|omo-senpi-gate-reviewer|final gate review").expect("valid regex"));

fn review_agent_type(tool_input: &Value, get_env: impl Fn(&str) -> Option<String> + Copy) -> Option<String> {
    let obj = tool_input.as_object()?;
    if let Some(agent_type) = obj.get("agent_type").and_then(|v| v.as_str()) {
        if !ALL_ROLES.contains(&agent_type) {
            return None;
        }
        return Some(active_surface_reviewer_alias(agent_type, get_env));
    }

    let message = obj.get("message").and_then(|v| v.as_str())?;
    let normalized = message.to_lowercase();

    if let Some(caps) = RE_EXPLICIT_ROLE.captures(&normalized) {
        if let Some(name) = caps.get(1) {
            return Some(active_surface_reviewer_alias(name.as_str(), get_env));
        }
    }

    if RE_ACT_AS.is_match(&normalized) {
        return None;
    }

    for name in ALL_ROLES.iter() {
        if normalized.contains(name) {
            return Some(active_surface_reviewer_alias(name, get_env));
        }
    }

    if RE_GATE.is_match(message) {
        Some(reviewer_roles_for(crate::surface::resolve_toolkit_surface_with_env(get_env)).gate_review.to_string())
    } else {
        None
    }
}

fn active_surface_reviewer_alias(reviewer: &str, get_env: impl Fn(&str) -> Option<String>) -> String {
    let canonical = canonical_reviewer_agent_name(reviewer);
    let active_roles = reviewer_roles_for(crate::surface::resolve_toolkit_surface_with_env(get_env));
    for roles in crate::surface::all_reviewer_roles() {
        if canonical == roles.code_review {
            return active_roles.code_review.to_string();
        }
        if canonical == roles.manual_qa {
            return active_roles.manual_qa.to_string();
        }
        if canonical == roles.gate_review {
            return active_roles.gate_review.to_string();
        }
    }
    canonical.to_string()
}

fn deny(reason: &str) -> String {
    let out = serde_json::json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": "deny",
            "permissionDecisionReason": reason,
            "additionalContext": reason,
        }
    });
    let mut s = serde_json::to_string(&out).unwrap_or_default();
    s.push('\n');
    s
}

fn fan_out_limit(get_env: impl Fn(&str) -> Option<String>) -> u32 {
    get_env("OMO_SPAWN_FANOUT_LIMIT")
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_FANOUT_LIMIT)
}

fn review_spawn_limit(get_env: impl Fn(&str) -> Option<String>) -> u32 {
    get_env("OMO_ULW_LOOP_REVIEW_SPAWN_LIMIT")
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_REVIEW_SPAWN_LIMIT)
}
