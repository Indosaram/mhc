//! `dag/results.ts`: durable copy of a terminal node's response (+ run_stats sidecar).
// allow: SIZE_OK - persist/read of the node result and its stats sidecar share one artifact contract.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::dag::store::DagFileStore;
use crate::dag::types::{DagNodeId, DagRunId, SchemaVersion1};
use crate::state::{TaskRecord, TaskRunStats};

/// A durable copy: relative path (from the task state dir), sha256, and byte length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagResultArtifactRef {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// The journaled description of the durable copy: the response artifact plus the optional
/// run_stats sidecar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagNodeResultArtifact {
    pub relative_path: String,
    pub sha256: String,
    pub bytes: u64,
    pub stats: Option<DagResultArtifactRef>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagNodeResultPersistOutcome {
    Persisted { artifact: DagNodeResultArtifact },
    Failed { diagnostic: crate::dag::store::DagStoreDiagnostic },
}

#[derive(Debug, Clone, PartialEq)]
pub struct DagNodeResultRead {
    pub output: String,
    pub run_stats: Option<TaskRunStats>,
}

pub struct DagNodeResultPersistInput<'a> {
    pub store: &'a DagFileStore,
    pub run_id: DagRunId,
    pub node_id: DagNodeId,
    pub record: &'a TaskRecord,
    pub now: Option<&'a dyn Fn() -> i64>,
}

pub struct DagNodeResultReadInput<'a> {
    pub store: &'a DagFileStore,
    pub run_id: &'a str,
    pub node_id: &'a str,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatsSidecar {
    schema_version: SchemaVersion1,
    run_id: DagRunId,
    node_id: DagNodeId,
    run_stats: TaskRunStats,
}

/// Copies a terminal node's final response (and run_stats sidecar) into the DAG result store.
///
/// Called SYNCHRONOUSLY inside the terminal-transition journal mutation: residency eviction drops
/// terminal idle TaskRecords, and the record itself expires on the task TTL, so a lazy copy would
/// lose the output. Single attempt only - a failed copy returns a `journal_corrupt` diagnostic for
/// the caller to journal, and the run continues.
pub fn persist_dag_node_result(input: DagNodeResultPersistInput<'_>) -> DagNodeResultPersistOutcome {
    let output_path = input.store.paths.result(&input.run_id, &input.node_id);
    match persist(&input, &output_path) {
        Ok(artifact) => DagNodeResultPersistOutcome::Persisted { artifact },
        Err(message) => {
            let now = input.now.map_or_else(system_now, |now| now());
            DagNodeResultPersistOutcome::Failed {
                diagnostic: crate::dag::store::DagStoreDiagnostic::JournalCorrupt {
                    run_id: Some(input.run_id.clone()),
                    path: output_path.to_string_lossy().into_owned(),
                    message: format!(
                        "failed to persist dag node result for \"{}\": {message}",
                        input.node_id
                    ),
                    at: to_iso(now),
                },
            }
        }
    }
}

fn persist(
    input: &DagNodeResultPersistInput<'_>,
    output_path: &Path,
) -> Result<DagNodeResultArtifact, String> {
    let output = input.record.final_response.clone().unwrap_or_default();
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    write_artifact(output_path, output.as_bytes()).map_err(|error| error.to_string())?;
    let stats = write_stats_sidecar(input, &stats_path(output_path)).map_err(|error| error.to_string())?;
    Ok(DagNodeResultArtifact {
        stats,
        ..artifact_ref(&input.store.state_dir, output_path, output.as_bytes())
    })
}

fn write_stats_sidecar(
    input: &DagNodeResultPersistInput<'_>,
    path: &Path,
) -> std::io::Result<Option<DagResultArtifactRef>> {
    let Some(run_stats) = input.record.run_stats.clone() else {
        return Ok(None);
    };
    let sidecar = StatsSidecar {
        schema_version: SchemaVersion1,
        run_id: input.run_id.clone(),
        node_id: input.node_id.clone(),
        run_stats,
    };
    // The sidecar shape is a fixed, always-serializable struct.
    let serialized = serde_json::to_string(&sidecar).unwrap_or_default();
    write_artifact(path, serialized.as_bytes())?;
    Ok(Some(artifact_ref_from(&input.store.state_dir, path, serialized.as_bytes())))
}

fn write_artifact(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new().write(true).create(true).truncate(true).open(path)?;
    file.write_all(contents)?;
    file.sync_all()
}

fn artifact_ref(state_dir: &Path, path: &Path, contents: &[u8]) -> DagNodeResultArtifact {
    let reference = artifact_ref_from(state_dir, path, contents);
    DagNodeResultArtifact {
        relative_path: reference.relative_path,
        sha256: reference.sha256,
        bytes: reference.bytes,
        stats: None,
    }
}

fn artifact_ref_from(state_dir: &Path, path: &Path, contents: &[u8]) -> DagResultArtifactRef {
    DagResultArtifactRef {
        relative_path: relative_to(state_dir, path),
        sha256: hex_sha256(contents),
        bytes: contents.len() as u64,
    }
}

fn relative_to(base: &Path, path: &Path) -> String {
    pathdiff(base, path).to_string_lossy().into_owned()
}

/// `node:path` `relative(from, to)`: both paths resolved beforehand by the store, so a plain
/// component-wise strip is sufficient here.
fn pathdiff(from: &Path, to: &Path) -> PathBuf {
    match to.strip_prefix(from) {
        Ok(stripped) => stripped.to_path_buf(),
        Err(_) => to.to_path_buf(),
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn stats_path(output_path: &Path) -> PathBuf {
    let stem = output_path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();
    output_path.with_file_name(format!("{stem}.stats.json"))
}

/// Reads a completed node's durable output for resume reuse. Reads ONLY the persisted artifacts,
/// never `TaskRecord.final_response` / `run_stats`, so reuse survives the task TTL sweep.
pub fn read_dag_node_result(input: DagNodeResultReadInput<'_>) -> Option<DagNodeResultRead> {
    let output_path = input.store.paths.result(input.run_id, input.node_id);
    let output = read_text_file(&output_path)?;
    let run_stats = read_stats(&stats_path(&output_path));
    Some(DagNodeResultRead { output, run_stats })
}

fn read_stats(path: &Path) -> Option<TaskRunStats> {
    let raw = read_text_file(path)?;
    let sidecar: StatsSidecar = serde_json::from_str(&raw).ok()?;
    Some(sidecar.run_stats)
}

fn read_text_file(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

fn system_now() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn to_iso(millis: i64) -> String {
    let secs = millis.div_euclid(1000);
    let nanos = millis.rem_euclid(1000) * 1_000_000;
    chrono::Utc
        .timestamp_opt(secs, nanos as u32)
        .single()
        .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_default()
}

use chrono::TimeZone;

#[cfg(test)]
#[path = "results_tests.rs"]
mod tests;
