//! Argument parsing for ulw-loop CLI.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Read};
use std::path::Path;
use serde_json::Value;

use crate::runtime::UlwLoopError;

pub struct RecordEvidenceCliArgs {
    pub goal_id: String,
    pub criterion_id: String,
    pub status: String,
    pub evidence: String,
    pub notes: Option<String>,
}

const VALUE_FLAGS: [&str; 24] = [
    "--brief", "--brief-file", "--session-id", "--codex-goal-mode", "--validation-batch-json",
    "--goal", "--goal-id", "--criterion-id", "--status", "--evidence", "--notes",
    "--codex-goal-json", "--quality-gate-json", "--kind", "--rationale", "--title",
    "--objective", "--target-goal-id", "--source", "--after-json", "--directive-json",
    "--directive-file", "--idempotency-key", "--proposals-json",
];

const SUBCOMMANDS: [&str; 9] = [
    "create-goals", "status", "complete-goals", "criteria", "record-evidence",
    "checkpoint", "steer", "add-goal", "record-review-blockers",
];

pub fn has_flag(argv: &[String], flag: &str) -> bool {
    argv.iter().any(|arg| arg == flag)
}

pub fn read_value(argv: &[String], flag: &str) -> Option<String> {
    if let Some(pos) = argv.iter().position(|a| a == flag) {
        if let Some(next) = argv.get(pos + 1) {
            if !next.starts_with("--") {
                return Some(next.clone());
            }
        }
        return None;
    }
    let prefix = format!("{flag}=");
    for arg in argv {
        if let Some(rest) = arg.strip_prefix(&prefix) {
            return Some(rest.to_string());
        }
    }
    None
}

pub fn read_repeated(argv: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    let prefix = format!("{flag}=");
    let mut i = 0;
    while i < argv.len() {
        let arg = &argv[i];
        if arg == flag && i + 1 < argv.len() && !argv[i + 1].starts_with("--") {
            values.push(argv[i + 1].clone());
            i += 2;
            continue;
        } else if let Some(rest) = arg.strip_prefix(&prefix) {
            values.push(rest.to_string());
        }
        i += 1;
    }
    values
}

pub fn parse_goal_arg(argv: &[String]) -> Option<String> {
    read_value(argv, "--goal-id").or_else(|| read_value(argv, "--goal"))
}

pub fn read_stdin() -> io::Result<String> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

pub fn positional_text(argv: &[String]) -> String {
    let mut words = Vec::new();
    let start = if argv.first().is_some_and(|s| SUBCOMMANDS.contains(&s.as_str())) {
        1
    } else {
        0
    };

    let mut i = start;
    while i < argv.len() {
        let arg = &argv[i];
        if VALUE_FLAGS.contains(&arg.as_str()) {
            i += 2;
            continue;
        }
        if arg.starts_with("--") {
            i += 1;
            continue;
        }
        words.push(arg.clone());
        i += 1;
    }
    words.join(" ").trim().to_string()
}

fn looks_like_json(value: &str) -> bool {
    let trimmed = value.trim();
    trimmed.starts_with('{') || trimmed.starts_with('[')
}

pub fn read_json_input(value: Option<&str>) -> Result<Option<Value>, UlwLoopError> {
    let Some(val) = value else {
        return Ok(None);
    };
    if looks_like_json(val) {
        serde_json::from_str(val).map(Some).map_err(|e| {
            UlwLoopError::new(
                format!("Invalid JSON input: {e}"),
                "ULW_LOOP_JSON_INPUT_INVALID",
            )
        })
    } else {
        let content = fs::read_to_string(val).map_err(|e| {
            UlwLoopError::new(
                format!("Invalid JSON input: {e}"),
                "ULW_LOOP_JSON_INPUT_INVALID",
            )
        })?;
        serde_json::from_str(&content).map(Some).map_err(|e| {
            UlwLoopError::new(
                format!("Invalid JSON input: {e}"),
                "ULW_LOOP_JSON_INPUT_INVALID",
            )
        })
    }
}

pub fn parse_codex_goal_json(value: Option<&str>) -> Result<Option<String>, UlwLoopError> {
    let Some(val) = value else {
        return Ok(None);
    };
    let raw = if looks_like_json(val) {
        val.to_string()
    } else {
        fs::read_to_string(val).map_err(|_| {
            UlwLoopError::new(
                "Invalid --codex-goal-json: neither valid JSON nor a readable path",
                "ULW_LOOP_CODEX_GOAL_JSON_INVALID",
            )
        })?
    };
    serde_json::from_str::<Value>(&raw).map_err(|e| {
        UlwLoopError::new(
            format!("Invalid --codex-goal-json: {e}"),
            "ULW_LOOP_CODEX_GOAL_JSON_INVALID",
        )
    })?;
    Ok(Some(raw))
}

fn required(argv: &[String], flag: &str, code: &str) -> Result<String, UlwLoopError> {
    if let Some(val) = read_value(argv, flag) {
        let trimmed = val.trim().to_string();
        if !trimmed.is_empty() {
            return Ok(trimmed);
        }
    }
    Err(UlwLoopError::new(format!("Missing {flag}."), code))
}

fn evidence_status(value: &str) -> Result<String, UlwLoopError> {
    match value {
        "pass" | "fail" | "blocked" => Ok(value.to_string()),
        _ => Err(UlwLoopError::new(
            "Invalid --status; expected pass, fail, or blocked.",
            "ULW_LOOP_EVIDENCE_STATUS_INVALID",
        )),
    }
}

pub fn parse_record_evidence_args(argv: &[String]) -> Result<RecordEvidenceCliArgs, UlwLoopError> {
    let goal_id = required(argv, "--goal-id", "ULW_LOOP_GOAL_ID_REQUIRED")?;
    let criterion_id = required(argv, "--criterion-id", "ULW_LOOP_CRITERION_ID_REQUIRED")?;
    let raw_status = required(argv, "--status", "ULW_LOOP_EVIDENCE_STATUS_REQUIRED")?;
    let status = evidence_status(&raw_status)?;
    let evidence = required(argv, "--evidence", "ULW_LOOP_EVIDENCE_REQUIRED")?;
    let notes = read_value(argv, "--notes").map(|n| n.trim().to_string()).filter(|n| !n.is_empty());

    Ok(RecordEvidenceCliArgs {
        goal_id,
        criterion_id,
        status,
        evidence,
        notes,
    })
}
