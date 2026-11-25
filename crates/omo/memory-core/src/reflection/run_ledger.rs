//! Strict reservation run-ledger parser shared by reconciliation and the stale-lock reaper.

use serde_json::{Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationRunLedger {
    pub raw: Map<String, Value>,
    pub run_id: String,
    pub kind: String,
    pub trigger: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservationLedgerError(pub &'static str);

impl std::fmt::Display for ReservationLedgerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for ReservationLedgerError {}

fn invalid(msg: &'static str) -> ReservationLedgerError {
    ReservationLedgerError(msg)
}

fn one_of(value: Option<&Value>, allowed: &[&str]) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|s| allowed.contains(&s))
}

fn optional_pid(value: Option<&Value>) -> bool {
    match value {
        None => true,
        Some(v) => v
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n > 0.0),
    }
}

fn optional_identity(value: Option<&Value>) -> bool {
    matches!(value, None | Some(Value::Null) | Some(Value::String(_)))
}

fn optional_positive_int(value: Option<&Value>) -> bool {
    match value {
        None => true,
        Some(v) => v
            .as_f64()
            .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n > 0.0),
    }
}

/// Strictly validates a parsed JSON value as a reflection/dream `ReservationRunLedger`.
pub fn parse_reservation_run_ledger(
    value: &Value,
) -> Result<ReservationRunLedger, ReservationLedgerError> {
    let Some(record) = value.as_object() else {
        return Err(invalid("Invalid reservation run ledger"));
    };
    if record.get("version").and_then(Value::as_f64) != Some(1.0)
        || !record.get("runId").is_some_and(Value::is_string)
    {
        return Err(invalid("Invalid reservation run ledger identity"));
    }
    if !one_of(record.get("kind"), &["reflection", "dream"]) {
        return Err(invalid("Invalid reservation run kind"));
    }
    if !one_of(
        record.get("trigger"),
        &["step-count", "compaction", "manual", "dream"],
    ) {
        return Err(invalid("Invalid reservation run trigger"));
    }
    let kind = record["kind"].as_str().unwrap_or_default().to_owned();
    let trigger = record["trigger"].as_str().unwrap_or_default().to_owned();
    let is_dream = kind == "dream";
    if is_dream != (trigger == "dream") {
        return Err(invalid("Reservation run kind and trigger disagree"));
    }
    if is_dream
        && !one_of(
            record.get("origin"),
            &["manual", "idle", "shutdown", "pressure"],
        )
    {
        return Err(invalid("Dream run origin is required"));
    }
    if [
        "startedAt",
        "worktreeDir",
        "worktreeBranch",
        "baseSha",
        "gitFilePath",
        "gitFileSnapshot",
        "commonConfigPath",
    ]
    .iter()
    .any(|field| !record.get(*field).is_some_and(Value::is_string))
    {
        return Err(invalid("Invalid reservation run paths"));
    }
    if ["hardDeadlineAt", "terminationGraceMs", "deadlineAt"]
        .iter()
        .any(|field| {
            !record
                .get(*field)
                .and_then(Value::as_f64)
                .is_some_and(f64::is_finite)
        })
    {
        return Err(invalid("Invalid reservation run deadline"));
    }
    let hard = record["hardDeadlineAt"].as_f64().unwrap_or(0.0);
    let grace = record["terminationGraceMs"].as_f64().unwrap_or(0.0);
    if record["deadlineAt"].as_f64() != Some(hard + grace) {
        return Err(invalid("Reservation run deadline mismatch"));
    }
    if !one_of(record.get("mergePolicy"), &["auto", "integration"]) {
        return Err(invalid("Invalid reservation merge policy"));
    }
    if let Some(target_doc) = record.get("targetDoc")
        && (!is_dream || !target_doc.is_string())
    {
        return Err(invalid("Invalid dream target document"));
    }
    for field in ["systemTokenBudget", "systemTokenTarget"] {
        if let Some(v) = record.get(field)
            && (!is_dream
                || !v
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && n.fract() == 0.0 && n > 0.0))
        {
            return Err(invalid("Invalid dream token budget field"));
        }
    }
    if record.get("systemTokenBudget").is_some() != record.get("systemTokenTarget").is_some() {
        return Err(invalid("Dream token budget metadata is incomplete"));
    }
    if !optional_pid(record.get("pid"))
        || !optional_identity(record.get("processStart"))
        || !optional_pid(record.get("childPid"))
        || !optional_identity(record.get("childProcessStart"))
    {
        return Err(invalid("Invalid reservation process identity"));
    }
    if !optional_positive_int(record.get("attempt")) {
        return Err(invalid("Invalid reservation run attempt"));
    }
    for field in [
        "model",
        "thinking",
        "category",
        "validatedTipSha",
        "integrationSha",
        "finalizeReason",
        "finalizeDetail",
        "finalizedAt",
    ] {
        if record.get(field).is_some_and(|v| !v.is_string()) {
            return Err(invalid("Invalid reservation run string field"));
        }
    }
    if let Some(ids) = record.get("conversationIds")
        && !ids
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    {
        return Err(invalid("Invalid reservation run conversations"));
    }
    for field in ["launching", "queued", "cleanupIncomplete"] {
        if record.get(field).is_some_and(|v| !v.is_boolean()) {
            return Err(invalid("Invalid reservation run boolean state"));
        }
    }
    if !optional_identity(record.get("commonConfigSnapshot")) {
        return Err(invalid("Invalid common config snapshot"));
    }
    if let Some(phase) = record.get("finalizePhase")
        && !one_of(Some(phase), &["validated", "integrated", "settled"])
    {
        return Err(invalid("Invalid finalization phase"));
    }
    if let Some(paths) = record.get("validatedChangedPaths")
        && !paths
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    {
        return Err(invalid("Invalid validated changed paths"));
    }
    if let Some(outcome) = record.get("finalizeOutcome")
        && !one_of(
            Some(outcome),
            &[
                "merged",
                "no_changes",
                "parent_dirty",
                "dirty_uncommitted",
                "merge_conflict",
                "admin_tamper",
                "timed_out",
                "failed",
            ],
        )
    {
        return Err(invalid("Invalid finalization outcome"));
    }
    let run_id = record["runId"].as_str().unwrap_or_default().to_owned();
    Ok(ReservationRunLedger {
        raw: record.clone(),
        run_id,
        kind,
        trigger,
    })
}
