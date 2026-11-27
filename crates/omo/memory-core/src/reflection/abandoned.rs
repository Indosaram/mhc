//! Terminal sentinel for a reflection/dream run that will never publish (`abandoned.json`).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::reflection::reservation::write_json_atomic;

pub const ABANDONED_FILENAME: &str = "abandoned.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbandonedReason {
    LaunchInterrupted,
    ProcessesDead,
}

impl AbandonedReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::LaunchInterrupted => "launch_interrupted",
            Self::ProcessesDead => "processes_dead",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbandonedLauncher {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub process_start: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AbandonedRecord {
    pub version: u32,
    pub run_id: String,
    pub abandoned_at: String,
    pub reason: AbandonedReason,
    /// Identity a receipt and backfill both compute: `ledger.startedAt`, else `reservedAt`.
    pub generation: String,
    pub kind: String,
    pub trigger: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launcher: Option<AbandonedLauncher>,
}

/// Writes `abandoned.json` inside `run_dir`; nothing already in `run_dir` is touched.
pub fn write_abandoned(run_dir: &Path, record: &AbandonedRecord) -> std::io::Result<()> {
    write_json_atomic(&run_dir.join(ABANDONED_FILENAME), record)
}
