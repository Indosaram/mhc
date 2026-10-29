//! Codex goal snapshot parsing and reconciliation.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use serde_json::Value;

use crate::runtime::UlwLoopError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CodexGoalSnapshotStatus {
    Active,
    Complete,
    Paused,
    UsageLimited,
    BudgetLimited,
    Cancelled,
    Failed,
    Unknown,
}

impl CodexGoalSnapshotStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Complete => "complete",
            Self::Paused => "paused",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Clone, Debug)]
pub struct CodexGoalSnapshot {
    pub available: bool,
    pub objective: Option<String>,
    pub status: CodexGoalSnapshotStatus,
    pub raw: Value,
}

#[derive(Clone, Debug)]
pub struct CodexGoalReconciliation {
    pub ok: bool,
    pub snapshot: CodexGoalSnapshot,
    pub warnings: Vec<String>,
    pub next_actions: Vec<String>,
    pub errors: Vec<String>,
    pub unacknowledged_objective: Option<String>,
}

pub struct ReconcileCodexGoalOptions<'a> {
    pub expected_objective: &'a str,
    pub accepted_objectives: Option<&'a [String]>,
    pub acknowledged_objectives: Option<&'a [String]>,
}

fn normalize_status(value: Option<&str>) -> CodexGoalSnapshotStatus {
    let status = value.unwrap_or("").trim().to_ascii_lowercase();
    match status.as_str() {
        "complete" | "completed" | "done" => CodexGoalSnapshotStatus::Complete,
        "cancelled" | "canceled" => CodexGoalSnapshotStatus::Cancelled,
        "failed" | "failure" => CodexGoalSnapshotStatus::Failed,
        "paused" => CodexGoalSnapshotStatus::Paused,
        "usage_limited" => CodexGoalSnapshotStatus::UsageLimited,
        "budget_limited" => CodexGoalSnapshotStatus::BudgetLimited,
        "active" | "in_progress" | "pending" | "running" => CodexGoalSnapshotStatus::Active,
        _ => CodexGoalSnapshotStatus::Unknown,
    }
}

fn normalize_objective(value: &str) -> String {
    let mut words = Vec::new();
    for word in value.split_whitespace() {
        words.push(word);
    }
    words.join(" ")
}

pub fn parse_codex_goal_snapshot(value: &Value) -> CodexGoalSnapshot {
    let root = value.as_object();
    let goal_value = root.and_then(|r| r.get("goal")).unwrap_or(value);

    if goal_value.is_null() || goal_value.as_bool() == Some(false) {
        return CodexGoalSnapshot {
            available: false,
            objective: None,
            status: CodexGoalSnapshotStatus::Unknown,
            raw: value.clone(),
        };
    }

    let goal_obj = goal_value.as_object();
    let get_str = |key: &str| -> Option<String> {
        goal_obj
            .and_then(|g| g.get(key))
            .or_else(|| root.and_then(|r| r.get(key)))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    };

    let objective = get_str("objective")
        .or_else(|| get_str("goal"))
        .or_else(|| get_str("description"))
        .or_else(|| get_str("title"));

    let status_str = get_str("status");
    let status = normalize_status(status_str.as_deref());

    let available = objective.is_some() || status != CodexGoalSnapshotStatus::Unknown;

    CodexGoalSnapshot {
        available,
        objective,
        status,
        raw: value.clone(),
    }
}

pub fn read_codex_goal_snapshot_input(
    raw: Option<&str>,
    cwd: impl AsRef<Path>,
) -> Result<Option<CodexGoalSnapshot>, UlwLoopError> {
    let Some(raw_str) = raw else {
        return Ok(None);
    };
    let trimmed = raw_str.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
        return Ok(Some(parse_codex_goal_snapshot(&parsed)));
    }

    let path = if Path::new(trimmed).is_absolute() {
        std::path::PathBuf::from(trimmed)
    } else {
        cwd.as_ref().join(trimmed)
    };

    if !path.exists() {
        return Err(UlwLoopError::new(
            format!("Codex goal snapshot is neither valid JSON nor a readable path: {trimmed}"),
            "CODEX_GOAL_SNAPSHOT_ERROR",
        ));
    }

    let content = fs::read_to_string(&path).map_err(|e| {
        UlwLoopError::new(
            format!("Codex goal snapshot path does not contain valid JSON: {trimmed} ({e})"),
            "CODEX_GOAL_SNAPSHOT_ERROR",
        )
    })?;

    let parsed: Value = serde_json::from_str(&content).map_err(|e| {
        UlwLoopError::new(
            format!("Codex goal snapshot path does not contain valid JSON: {trimmed} ({e})"),
            "CODEX_GOAL_SNAPSHOT_ERROR",
        )
    })?;

    Ok(Some(parse_codex_goal_snapshot(&parsed)))
}

pub fn reconcile_codex_goal_snapshot(
    snapshot: Option<&CodexGoalSnapshot>,
    options: &ReconcileCodexGoalOptions,
) -> CodexGoalReconciliation {
    let effective = snapshot.cloned().unwrap_or_else(|| CodexGoalSnapshot {
        available: false,
        objective: None,
        status: CodexGoalSnapshotStatus::Unknown,
        raw: Value::Null,
    });

    let mut errors = Vec::new();
    let mut warnings = Vec::new();
    let mut next_actions = Vec::new();

    let expected = options.expected_objective;
    if !effective.available {
        next_actions.push(format!(
            "call get_goal; if none, create_goal with codexObjective \"{expected}\" verbatim"
        ));
        return CodexGoalReconciliation {
            ok: errors.is_empty(),
            snapshot: effective,
            warnings,
            next_actions,
            errors,
            unacknowledged_objective: None,
        };
    }

    let mut accepted = BTreeSet::new();
    accepted.insert(normalize_objective(expected));
    if let Some(list) = options.accepted_objectives {
        for obj in list {
            accepted.insert(normalize_objective(obj));
        }
    }

    let mut acknowledged = BTreeSet::new();
    if let Some(list) = options.acknowledged_objectives {
        for obj in list {
            acknowledged.insert(normalize_objective(obj));
        }
    }

    let actual = normalize_objective(effective.objective.as_deref().unwrap_or(""));
    let mut unack = None;
    if !actual.is_empty() && !accepted.contains(&actual) && !acknowledged.contains(&actual) {
        warnings.push(format!(
            "driver_objective_differs: expected \"{expected}\", got \"{actual}\"."
        ));
        unack = Some(actual);
    }

    match effective.status {
        CodexGoalSnapshotStatus::Paused
        | CodexGoalSnapshotStatus::UsageLimited
        | CodexGoalSnapshotStatus::BudgetLimited => {
            next_actions.push("/goal resume or raise the budget".to_string());
        }
        CodexGoalSnapshotStatus::Complete => {
            next_actions.push(format!(
                "driver closed early: call create_goal with codexObjective \"{expected}\" verbatim"
            ));
        }
        _ => {}
    }

    CodexGoalReconciliation {
        ok: errors.is_empty(),
        snapshot: effective,
        warnings,
        next_actions,
        errors,
        unacknowledged_objective: unack,
    }
}

pub fn format_codex_goal_reconciliation(reconciliation: &CodexGoalReconciliation) -> String {
    let mut parts = Vec::new();
    parts.extend(reconciliation.errors.iter().cloned());
    parts.extend(reconciliation.next_actions.iter().cloned());
    parts.extend(reconciliation.warnings.iter().cloned());
    parts.join(" ")
}
