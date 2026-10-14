//! Commit log reading and reconciliation for ulw-loop.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::domain_types::{UlwLoopLedgerEntry, UlwLoopPlan};
use crate::paths::{ulw_loop_dir, UlwLoopScope};
use crate::runtime::UlwLoopError;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanCommitRecord {
    pub version: u32,
    pub revision: u64,
    pub plan: UlwLoopPlan,
    pub ledger: Vec<UlwLoopLedgerEntry>,
}

pub fn read_optional(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn log_names(dir: impl AsRef<Path>) -> Vec<String> {
    let revisions_dir = dir.as_ref().join("revisions");
    let Ok(entries) = fs::read_dir(revisions_dir) else {
        return Vec::new();
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && name.len() >= 13 {
            let stem = &name[..name.len() - 5];
            if stem.chars().all(|c| c.is_ascii_digit()) && stem.len() >= 8 {
                names.push(name);
            }
        }
    }
    names.sort();
    names
}

fn read_record(dir: impl AsRef<Path>, name: &str) -> Option<PlanCommitRecord> {
    let path = dir.as_ref().join("revisions").join(name);
    let content = fs::read_to_string(path).ok()?;
    let record: PlanCommitRecord = serde_json::from_str(&content).ok()?;
    if record.version == 1 && record.plan.version == 1 {
        Some(record)
    } else {
        None
    }
}

pub fn read_records(dir: impl AsRef<Path>) -> Vec<PlanCommitRecord> {
    let mut records = Vec::new();
    for name in log_names(&dir) {
        if let Some(record) = read_record(&dir, &name) {
            records.push(record);
        }
    }
    records.sort_by_key(|r| r.revision);
    records
}

pub fn read_newest_record(dir: impl AsRef<Path>) -> Option<PlanCommitRecord> {
    let mut names = log_names(&dir);
    names.sort_by(|a, b| {
        let parse_num = |s: &str| -> u64 {
            let stem = s.strip_suffix(".json").unwrap_or(s);
            stem.parse().unwrap_or(0)
        };
        parse_num(b).cmp(&parse_num(a))
    });

    for name in names {
        let path = dir.as_ref().join("revisions").join(&name);
        if fs::metadata(&path).is_ok_and(|metadata| metadata.is_dir()) {
            return None;
        }
        if let Some(record) = read_record(&dir, &name) {
            return Some(record);
        }
    }
    None
}

pub fn reconcile_plan(dir: impl AsRef<Path>) -> Option<UlwLoopPlan> {
    let raw = read_optional(dir.as_ref().join("goals.json"));
    // Pin `plan-io.ts:73-81`: the raw document must be `version: 1` with a `goals` array before it
    // counts as a plan; serde defaults alone would accept a goals-less `{"version":1}` as empty.
    let cached: Option<UlwLoopPlan> = raw
        .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
        .filter(|value| value.get("version").and_then(serde_json::Value::as_u64) == Some(1)
            && value.get("goals").is_some_and(serde_json::Value::is_array))
        .and_then(|value| serde_json::from_value(value).ok());
    let latest = read_newest_record(&dir);

    match (cached, latest) {
        (None, None) => None,
        (Some(c), None) => Some(c),
        (None, Some(l)) => Some(l.plan),
        (Some(c), Some(l)) => {
            if c.revision.unwrap_or(0) > l.revision {
                return Some(c);
            }
            if c.revision.is_none() || c.revision == Some(l.revision) {
                let published: BTreeSet<_> = l.plan.goals.iter().map(|g| &g.id).collect();
                if c.goals.iter().any(|g| !published.contains(&g.id)) {
                    let mut updated = c;
                    updated.revision = Some(l.revision);
                    return Some(updated);
                }
            }
            Some(l.plan)
        }
    }
}

pub fn assert_projection_complete(
    plan: &UlwLoopPlan,
    ledger: &[UlwLoopLedgerEntry],
) -> Result<(), UlwLoopError> {
    let present: BTreeSet<_> = plan.goals.iter().map(|g| &g.id).collect();
    let mut missing = BTreeSet::new();
    for entry in ledger {
        if entry.kind == "goal_added" {
            if let Some(gid) = &entry.goal_id {
                if !present.contains(gid) {
                    missing.insert(gid.clone());
                }
            }
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    let missing_vec: Vec<_> = missing.into_iter().collect();
    let mut details = BTreeMap::new();
    details.insert("missingGoalIds".into(), serde_json::json!(missing_vec));
    Err(UlwLoopError::with_details(
        format!(
            "Refusing to rewrite goals.json: the ledger records goals the plan projection lacks ({}). Restore those goals into the newest revisions/ record (keeping plan.revision equal to its file number) before mutating this run.",
            missing_vec.join(", ")
        ),
        "ULW_LOOP_PROJECTION_TRUNCATED",
        details,
    ))
}

pub fn plan_exists(repo_root: impl AsRef<Path>, scope: Option<&UlwLoopScope>) -> bool {
    let dir = crate::paths::ulw_loop_dir(repo_root, scope);
    dir.join("goals.json").exists() || !log_names(&dir).is_empty()
}


