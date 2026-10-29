//! `dag/manager.ts`: run lifecycle - key idempotency, session ownership, and snapshot projection.
// allow: SIZE_OK - the run lifecycle surface keeps key idempotency, session ownership, and snapshot
// projection on one contract so callers cannot bypass a step.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::dag::fingerprint::{
    DagDefinitionFingerprintInputV1, DagNodeFingerprintInputV1, DagSchedulerContract,
    dag_definition_fingerprint,
};
use crate::dag::graph::{
    DagCompileError, DagCompileOptions, DagDefinition, DagNodeInput, compile_dag,
};
use crate::dag::journal::{DagJournalCheckpoint, DagJournalOptions, create_dag_journal};
use crate::dag::skills::{DagSkillManifest, read_dag_skill_manifest, write_dag_skill_manifest};
use crate::dag::store::{DagEventPage, DagEventReadOptions, DagFileStore, DagStoreError};
use crate::dag::types::{
    AmendRecord, DAG_SETTINGS_DEFAULTS, DagBottleneck, DagDiagnostic, DagEdge, DagEventLane,
    DagNode, DagNodeCounts, DagNodeId, DagNodeState, DagNodeTarget, DagRoute, DagRunEvent,
    DagRunEventPayload, DagRunEventType, DagRunId, DagRunSnapshot, DagRunStatus, DagSettings,
    DagWave, SchemaVersion1,
};

const LIST_DEFAULT_LIMIT: usize = 100;
const LIST_MAX_LIMIT: usize = 256;

const SCHEDULER_FINGERPRINT_INPUT: DagSchedulerContract = DagSchedulerContract {
    wave_admission: "dependency-frontier",
    failure_policy: "continue-independent",
    dependency_data: "filesystem-only",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DagManagerErrorCode {
    InvalidDefinition,
    DefinitionConflict,
    RunNotFound,
    RunNotOwned,
    InvalidArguments,
    InvalidAmendment,
    AmendRunningNode,
    RunStillActive,
}

pub const DAG_MANAGER_ERROR_CODES: [DagManagerErrorCode; 8] = [
    DagManagerErrorCode::InvalidDefinition,
    DagManagerErrorCode::DefinitionConflict,
    DagManagerErrorCode::RunNotFound,
    DagManagerErrorCode::RunNotOwned,
    DagManagerErrorCode::InvalidArguments,
    DagManagerErrorCode::InvalidAmendment,
    DagManagerErrorCode::AmendRunningNode,
    DagManagerErrorCode::RunStillActive,
];

impl DagManagerErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidDefinition => "invalid_definition",
            Self::DefinitionConflict => "definition_conflict",
            Self::RunNotFound => "run_not_found",
            Self::RunNotOwned => "run_not_owned",
            Self::InvalidArguments => "invalid_arguments",
            Self::InvalidAmendment => "invalid_amendment",
            Self::AmendRunningNode => "amend_running_node",
            Self::RunStillActive => "run_still_active",
        }
    }
}

/// Compile details and affected node identifiers carried by [`DagManagerError`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DagManagerErrorDetails {
    pub errors: Vec<DagCompileError>,
    pub diagnostics: Vec<DagDiagnostic>,
    pub node_ids: Vec<DagNodeId>,
}

/// Every `DagManager` rejection. `code` is the wire vocabulary the dag tool and RPC handlers
/// surface verbatim; `errors`/`diagnostics` carry compile detail for `invalid_definition`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct DagManagerError {
    pub code: DagManagerErrorCode,
    pub message: String,
    pub run_id: Option<DagRunId>,
    pub details: Box<DagManagerErrorDetails>,
}

impl DagManagerError {
    pub fn new(code: DagManagerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            run_id: None,
            details: Box::default(),
        }
    }

    pub fn with_run_id(mut self, run_id: DagRunId) -> Self {
        self.run_id = Some(run_id);
        self
    }

    pub fn with_node_ids(mut self, node_ids: Vec<DagNodeId>) -> Self {
        self.details.node_ids = node_ids;
        self
    }

    pub fn with_compile(
        mut self,
        errors: Vec<DagCompileError>,
        diagnostics: Vec<DagDiagnostic>,
    ) -> Self {
        self.details.errors = errors;
        self.details.diagnostics = diagnostics;
        self
    }

    pub fn errors(&self) -> &[DagCompileError] {
        &self.details.errors
    }

    pub fn diagnostics(&self) -> &[DagDiagnostic] {
        &self.details.diagnostics
    }

    pub fn node_ids(&self) -> &[DagNodeId] {
        &self.details.node_ids
    }
}

impl From<DagStoreError> for DagManagerError {
    fn from(error: DagStoreError) -> Self {
        Self::new(DagManagerErrorCode::InvalidArguments, error.to_string())
    }
}

/// Persisted per node at creation time. `prompt` is the submitted original (the only fingerprinted
/// text); `effective_prompt` is dispatch material filled by the skill materialization seam.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagPersistedNode {
    pub id: String,
    pub prompt: String,
    #[serde(flatten)]
    pub target: DagPersistedNodeTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depends_on: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_skills: Option<Vec<String>>,
    pub effective_prompt: String,
}

/// The wire shape of `DagNodeTarget`: category XOR subagent_type, model only alongside
/// subagent_type. Flattened onto [`DagPersistedNode`] exactly like the TS `DagNodeInput` union.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DagPersistedNodeTarget {
    Category {
        category: String,
    },
    SubagentType {
        subagent_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

fn persisted_target(target: &DagNodeTarget) -> DagPersistedNodeTarget {
    match target {
        DagNodeTarget::Category(category) => DagPersistedNodeTarget::Category {
            category: category.clone(),
        },
        DagNodeTarget::SubagentType {
            subagent_type,
            model,
        } => DagPersistedNodeTarget::SubagentType {
            subagent_type: subagent_type.clone(),
            model: model.clone(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagPersistedDefinition {
    pub key: String,
    pub name: String,
    pub nodes: Vec<DagPersistedNode>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DagRunRecordV1 {
    pub schema_version: SchemaVersion1,
    pub checkpoint_seq: u64,
    pub run_id: DagRunId,
    pub run_key: String,
    pub name: String,
    pub parent_session_id: String,
    pub root_session_id: String,
    pub definition_fingerprint: String,
    pub definition: DagPersistedDefinition,
    pub status: DagRunStatus,
    pub generation: u64,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    pub nodes: Vec<DagNode>,
    pub edges: Vec<DagEdge>,
    pub waves: Vec<DagWave>,
    pub critical_path: Vec<DagNodeId>,
    pub bottlenecks: Vec<DagBottleneck>,
    pub diagnostics: Vec<DagDiagnostic>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amend_history: Option<Vec<AmendRecord>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_holder_pid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_lease_holder_pid: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guidance: Option<Vec<crate::dag::types::Guidance>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_on_resume: Option<Vec<DagNodeId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_requested_nodes: Option<Vec<DagNodeId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub paused_child_acks: Option<HashMap<String, u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause: Option<crate::dag::types::DagRunPause>,
}

impl DagJournalCheckpoint for DagRunRecordV1 {
    fn checkpoint_seq(&self) -> u64 {
        self.checkpoint_seq
    }

    fn with_checkpoint_seq(&self, checkpoint_seq: u64) -> Self {
        Self {
            checkpoint_seq,
            ..self.clone()
        }
    }

    fn with_checkpoint_seq_owned(mut self, checkpoint_seq: u64) -> Self {
        self.checkpoint_seq = checkpoint_seq;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagRunSummary {
    pub run_id: DagRunId,
    pub run_key: String,
    pub name: String,
    pub parent_session_id: String,
    pub status: DagRunStatus,
    pub created_at: String,
    pub updated_at: String,
    pub counts: DagNodeCounts,
}

#[derive(Clone)]
struct CachedRunSummary {
    inode: u64,
    size: u64,
    modified: std::time::SystemTime,
    summary: DagRunSummary,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DagStartResult {
    pub reused: bool,
    pub snapshot: DagRunSnapshot,
}

pub struct DagRunHandle {
    pub run_id: DagRunId,
    manager: Arc<DagManagerInner>,
    parent_session_id: String,
}

/// The inner manager is not `Debug` (it owns closures and live stores), so the handle reports only
/// its own identity; tests use this to unwrap `Result<DagRunHandle, _>` errors.
impl std::fmt::Debug for DagRunHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DagRunHandle")
            .field("run_id", &self.run_id)
            .field("parent_session_id", &self.parent_session_id)
            .finish()
    }
}

impl DagRunHandle {
    pub fn snapshot(&self) -> Result<DagRunSnapshot, DagManagerError> {
        self.manager
            .owned_record(&self.run_id, &self.parent_session_id)
            .map(|record| project_snapshot_with_state_dir(record, Some(&self.manager.state_dir)))
    }
}

/// The todo 16 seam: skill resolution happens ONCE, at creation, and only fills effective_prompt.
/// It never feeds the fingerprint and is never re-run on reuse or resume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagSkillMaterialization {
    pub nodes: Vec<(String, String)>,
    pub diagnostics: Vec<DagDiagnostic>,
}

pub struct DagMaterializeSkillsInput<'a> {
    pub run_id: &'a DagRunId,
    pub definition: &'a DagDefinition,
    pub at: &'a str,
}

pub type DagMaterializeSkills = Arc<
    dyn Fn(DagMaterializeSkillsInput<'_>) -> Result<DagSkillMaterialization, DagManagerError>
        + Send
        + Sync,
>;

pub struct DagManagerOptions {
    pub store: Arc<DagFileStore>,
    pub new_run_id: Option<Arc<dyn Fn() -> DagRunId + Send + Sync>>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    pub materialize_skills: Option<DagMaterializeSkills>,
    pub settings: Option<DagSettings>,
}

pub struct DagHistoryParams {
    pub run_id: DagRunId,
    pub parent_session_id: String,
    pub since_seq: Option<u64>,
    pub limit: Option<usize>,
    pub lane: Option<DagEventLane>,
    pub types: Option<Vec<DagRunEventType>>,
    pub through_seq: Option<u64>,
}

pub struct DagStartParams {
    pub definition: DagDefinition,
    pub parent_session_id: String,
    pub root_session_id: String,
}

pub struct DagAmendParams {
    pub run_id: Option<DagRunId>,
    pub key: Option<String>,
    pub definition: DagDefinition,
    pub parent_session_id: String,
}

struct DagManagerInner {
    store: Arc<DagFileStore>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    new_run_id: Arc<dyn Fn() -> DagRunId + Send + Sync>,
    settings: DagSettings,
    materialize_skills: Option<DagMaterializeSkills>,
    summary_cache: std::sync::Mutex<HashMap<DagRunId, CachedRunSummary>>,
    state_dir: PathBuf,
}

impl DagManagerInner {
    fn owned_record(
        &self,
        run_id: &DagRunId,
        parent_session_id: &str,
    ) -> Result<DagRunRecordV1, DagManagerError> {
        let record = self
            .store
            .read_checkpoint::<DagRunRecordV1>(run_id)
            .map_err(DagManagerError::from)?
            .ok_or_else(|| {
                DagManagerError::new(
                    DagManagerErrorCode::RunNotFound,
                    format!("unknown dag run \"{run_id}\""),
                )
                .with_run_id(run_id.clone())
            })?;
        // Session ownership is absolute: a foreign caller that already knows the runId still gets
        // no data.
        if record.parent_session_id != parent_session_id {
            return Err(DagManagerError::new(
                DagManagerErrorCode::RunNotOwned,
                format!("dag run \"{run_id}\" belongs to another session"),
            )
            .with_run_id(run_id.clone()));
        }
        Ok(record)
    }
}

/// A run lifecycle manager: creation (with key idempotency), attach/snapshot/record projection,
/// per-session listing, and event history.
#[derive(Clone)]
pub struct DagManager {
    inner: Arc<DagManagerInner>,
}

pub fn create_dag_manager(options: DagManagerOptions) -> DagManager {
    let now = options.now.unwrap_or_else(|| {
        Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX))
    });
    let new_run_id = options
        .new_run_id
        .unwrap_or_else(|| Arc::new(|| format!("dag_{}", uuid_v4()) as DagRunId));
    let state_dir = options
        .store
        .paths
        .root
        .parent()
        .unwrap_or(&options.store.paths.root)
        .to_path_buf();
    let settings = DagSettings {
        max_nodes_per_run: options
            .settings
            .map_or(DAG_SETTINGS_DEFAULTS.max_nodes_per_run, |s| {
                s.max_nodes_per_run
            }),
        ..options.settings.unwrap_or(DAG_SETTINGS_DEFAULTS)
    };
    DagManager {
        inner: Arc::new(DagManagerInner {
            store: options.store,
            now,
            new_run_id,
            settings,
            materialize_skills: options.materialize_skills,
            summary_cache: std::sync::Mutex::new(HashMap::new()),
            state_dir,
        }),
    }
}

fn uuid_v4() -> String {
    let mut bytes = [0_u8; 16];
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut seed = nanos ^ (std::process::id() as u128) << 64;
    for byte in &mut bytes {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        *byte = (seed >> 96) as u8;
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

impl DagManager {
    pub fn start(&self, params: DagStartParams) -> Result<DagStartResult, DagManagerError> {
        start_run(params, &self.inner)
    }

    pub fn attach(
        &self,
        run_id: &DagRunId,
        parent_session_id: &str,
    ) -> Result<DagRunHandle, DagManagerError> {
        self.inner.owned_record(run_id, parent_session_id)?;
        Ok(DagRunHandle {
            run_id: run_id.clone(),
            manager: Arc::clone(&self.inner),
            parent_session_id: parent_session_id.to_string(),
        })
    }

    pub fn snapshot(
        &self,
        run_id: &DagRunId,
        parent_session_id: &str,
    ) -> Result<DagRunSnapshot, DagManagerError> {
        self.inner
            .owned_record(run_id, parent_session_id)
            .map(|record| project_snapshot_with_state_dir(record, Some(&self.inner.state_dir)))
    }

    pub fn record(
        &self,
        run_id: &DagRunId,
        parent_session_id: &str,
    ) -> Result<DagRunRecordV1, DagManagerError> {
        self.inner.owned_record(run_id, parent_session_id)
    }

    pub fn list(
        &self,
        parent_session_id: &str,
        limit: Option<usize>,
    ) -> Result<Vec<DagRunSummary>, DagManagerError> {
        let limit = resolve_limit(limit, LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT)?;
        let mut summaries: Vec<DagRunSummary> = Vec::new();
        // TS readDagDirectory: a runs directory that does not exist yet lists as empty.
        let entries = match fs::read_dir(&self.inner.store.paths.runs) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(DagManagerError::from(DagStoreError::from(error))),
        };
        let mut present = HashSet::new();
        for entry in entries {
            let entry = entry
                .map_err(DagStoreError::from)
                .map_err(DagManagerError::from)?;
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let run_id = stem.to_string();
            present.insert(run_id.clone());
            // TS runSummary statSync(throwIfNoEntry: false): a run pruned mid-list is skipped.
            let metadata = match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(DagManagerError::from(DagStoreError::from(error))),
            };
            let modified = metadata
                .modified()
                .map_err(DagStoreError::from)
                .map_err(DagManagerError::from)?;
            let inode = file_inode(&metadata);
            let mut cache = self
                .inner
                .summary_cache
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let summary = if let Some(cached) = cache.get(&run_id).filter(|cached| {
                cached.inode == inode
                    && cached.size == metadata.len()
                    && cached.modified == modified
            }) {
                cached.summary.clone()
            } else {
                let Some(record) = self
                    .inner
                    .store
                    .read_checkpoint::<DagRunRecordV1>(&run_id)
                    .map_err(DagManagerError::from)?
                else {
                    cache.remove(&run_id);
                    continue;
                };
                let summary = DagRunSummary {
                    run_id: record.run_id,
                    run_key: record.run_key,
                    name: record.name,
                    parent_session_id: record.parent_session_id,
                    status: record.status,
                    created_at: record.created_at,
                    updated_at: record.updated_at,
                    counts: count_nodes(&record.nodes),
                };
                cache.insert(
                    run_id.clone(),
                    CachedRunSummary {
                        inode,
                        size: metadata.len(),
                        modified,
                        summary: summary.clone(),
                    },
                );
                summary
            };
            drop(cache);
            if summary.parent_session_id != parent_session_id {
                continue;
            }
            summaries.push(summary);
        }
        self.inner
            .summary_cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|run_id, _| present.contains(run_id));
        summaries.sort_by(|a, b| {
            let by_updated = parse_date_ms(&b.updated_at).cmp(&parse_date_ms(&a.updated_at));
            if by_updated != std::cmp::Ordering::Equal {
                return by_updated;
            }
            a.run_id.cmp(&b.run_id)
        });
        summaries.truncate(limit);
        Ok(summaries)
    }

    pub fn amend(&self, params: DagAmendParams) -> Result<DagRunRecordV1, DagManagerError> {
        amend_run(params, &self.inner)
    }

    pub fn history(&self, params: DagHistoryParams) -> Result<DagEventPage, DagManagerError> {
        self.inner
            .owned_record(&params.run_id, &params.parent_session_id)?;
        let since_seq = params.since_seq.unwrap_or(0);
        let limit = resolve_limit(
            params.limit,
            self.inner.settings.history_default_limit,
            self.inner.settings.history_max_limit,
        )
        .map_err_with_run_id(&params.run_id)?;
        self.inner
            .store
            .read_events(
                &params.run_id,
                since_seq,
                &DagEventReadOptions {
                    limit,
                    lane: params.lane,
                    types: params.types,
                    through_seq: params.through_seq,
                },
            )
            .map_err(DagManagerError::from)
    }
}

struct StartContext<'a> {
    inner: &'a DagManagerInner,
}

fn start_run(
    params: DagStartParams,
    inner: &Arc<DagManagerInner>,
) -> Result<DagStartResult, DagManagerError> {
    let context = StartContext { inner };
    let at = crate::shared::iso_from_ms((context.inner.now)());
    let definition = params.definition;

    // (1) validate + compile. Any error diagnostic creates no run, no key file, and no event.
    let compiled = compile_dag(
        &definition,
        &DagCompileOptions {
            at: Some(at.clone()),
            settings: Some(context.inner.settings),
        },
    );
    if !compiled.ok {
        let message = compiled
            .errors
            .iter()
            .map(|error| error.message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(
            DagManagerError::new(DagManagerErrorCode::InvalidDefinition, message)
                .with_compile(compiled.errors, compiled.diagnostics),
        );
    }

    // (2) fingerprint the SUBMITTED definition: never effective_prompt, never skill content.
    let definition_fingerprint = fingerprint_definition(&definition);

    // (3)+(4) under the key lock so a concurrent same-key start can never create a second run.
    context
        .inner
        .store
        .with_key_lock(&params.parent_session_id, &definition.key, || {
            start_run_locked(
                &params.parent_session_id,
                &params.root_session_id,
                &definition,
                &definition_fingerprint,
                &at,
                &compiled,
                context.inner,
            )
        })
        .map_err(DagManagerError::from)?
}

#[allow(clippy::too_many_arguments)]
fn start_run_locked(
    parent_session_id: &str,
    root_session_id: &str,
    definition: &DagDefinition,
    definition_fingerprint: &str,
    at: &str,
    compiled: &crate::dag::graph::DagCompileResult,
    inner: &DagManagerInner,
) -> Result<DagStartResult, DagManagerError> {
    let existing_key = inner
        .store
        .read_key(parent_session_id, &definition.key)
        .map_err(DagManagerError::from)?;
    // A key whose run record is gone (retention pruned it) is stale, not a conflict: the key is
    // rewritten by the fresh run below. Conflict is reserved for a key with a LIVE divergent run.
    let existing = match &existing_key {
        Some(key) => inner
            .store
            .read_checkpoint::<DagRunRecordV1>(&key.run_id)
            .map_err(DagManagerError::from)?,
        None => None,
    };
    if let Some(existing) = existing {
        if existing.definition_fingerprint != definition_fingerprint {
            return Err(DagManagerError::new(
                DagManagerErrorCode::DefinitionConflict,
                format!(
                    "dag run key \"{}\" already exists with a different definition",
                    definition.key
                ),
            )
            .with_run_id(existing.run_id.clone()));
        }
        if let Some(key) = existing_key.as_ref()
            && key.definition_fingerprint.as_deref() != Some(definition_fingerprint)
        {
            return Err(DagManagerError::new(
                DagManagerErrorCode::DefinitionConflict,
                format!(
                    "dag run key \"{}\" has a mismatched stored fingerprint",
                    definition.key
                ),
            )
            .with_run_id(existing.run_id.clone()));
        }
        // Reuse never re-materializes skills: the run keeps its creation-time effective_prompt.
        return Ok(DagStartResult {
            reused: true,
            snapshot: project_snapshot_with_state_dir(existing, Some(&inner.state_dir)),
        });
    }

    let run_id = (inner.new_run_id)();
    let materialized = match inner.materialize_skills.as_ref() {
        Some(materialize) => Some(materialize(DagMaterializeSkillsInput {
            run_id: &run_id,
            definition,
            at,
        })?),
        None => None,
    };
    let effective_prompts: std::collections::HashMap<String, String> = materialized
        .as_ref()
        .map(|materialization| materialization.nodes.iter().cloned().collect())
        .unwrap_or_default();
    let nodes: Vec<DagPersistedNode> = definition
        .nodes
        .iter()
        .map(|node| persisted_node(node, &effective_prompts))
        .collect();
    let mut diagnostics = compiled.diagnostics.clone();
    if let Some(materialization) = &materialized {
        diagnostics.extend(materialization.diagnostics.clone());
    }
    let record = DagRunRecordV1 {
        schema_version: SchemaVersion1,
        checkpoint_seq: 0,
        run_id: run_id.clone(),
        run_key: definition.key.clone(),
        name: definition.name.clone(),
        parent_session_id: parent_session_id.to_string(),
        root_session_id: root_session_id.to_string(),
        definition_fingerprint: definition_fingerprint.to_string(),
        definition: DagPersistedDefinition {
            key: definition.key.clone(),
            name: definition.name.clone(),
            nodes,
        },
        status: DagRunStatus::Pending,
        generation: 1,
        created_at: at.to_string(),
        updated_at: at.to_string(),
        started_at: None,
        completed_at: None,
        nodes: compiled.nodes.clone(),
        edges: compiled.edges.clone(),
        waves: compiled.waves.clone(),
        critical_path: compiled.critical_path.clone(),
        bottlenecks: compiled.bottlenecks.clone(),
        diagnostics,
        amend_history: None,
        lease_holder_pid: None,
        previous_lease_holder_pid: None,
        guidance: None,
        retry_on_resume: None,
        cancel_requested_nodes: None,
        paused_child_acks: None,
        pause: None,
    };

    // Journal skeleton first: the checkpoint must exist before the key file can point at it, so a
    // crash between the two leaves an unkeyed run rather than a key pointing at nothing.
    inner
        .store
        .write_checkpoint(&run_id, &record)
        .map_err(DagManagerError::from)?;
    inner
        .store
        .write_key(&crate::dag::store::DagKeyRecord {
            schema_version: SchemaVersion1,
            parent_session_id: parent_session_id.to_string(),
            run_key: definition.key.clone(),
            run_id: run_id.clone(),
            definition_fingerprint: Some(definition_fingerprint.to_string()),
        })
        .map_err(DagManagerError::from)?;
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&inner.store),
        run_id: run_id.clone(),
        initial_checkpoint: record,
        apply_event: Arc::new(apply_run_event),
        subscriber_ring: None,
        now: Some(Arc::clone(&inner.now)),
    })
    .map_err(DagManagerError::from)?;
    journal.set_skip_duplicate(Arc::new(skip_duplicate_terminal_transition));
    journal
        .append(DagRunEventPayload::RunCreated {
            run_key: definition.key.clone(),
            name: definition.name.clone(),
            definition_fingerprint: definition_fingerprint.to_string(),
            node_count: compiled.nodes.len(),
            edge_count: compiled.edges.len(),
        })
        .map_err(DagManagerError::from)?;
    Ok(DagStartResult {
        reused: false,
        snapshot: project_snapshot_with_state_dir(journal.snapshot(), Some(&inner.state_dir)),
    })
}

fn amend_run(
    params: DagAmendParams,
    inner: &DagManagerInner,
) -> Result<DagRunRecordV1, DagManagerError> {
    if params.run_id.is_some() == params.key.is_some() {
        return Err(DagManagerError::new(
            DagManagerErrorCode::InvalidArguments,
            "amend requires exactly one of runId or key",
        ));
    }
    let selected = match params.run_id {
        Some(run_id) => run_id,
        None => inner
            .store
            .read_key(
                &params.parent_session_id,
                params.key.as_deref().unwrap_or_default(),
            )
            .map_err(DagManagerError::from)?
            .map(|key| key.run_id)
            .ok_or_else(|| {
                DagManagerError::new(
                    DagManagerErrorCode::RunNotFound,
                    format!(
                        "unknown dag run key \"{}\"",
                        params.key.as_deref().unwrap_or_default()
                    ),
                )
            })?,
    };
    let old = inner
        .store
        .read_checkpoint::<DagRunRecordV1>(&selected)
        .map_err(DagManagerError::from)?
        .ok_or_else(|| {
            DagManagerError::new(
                DagManagerErrorCode::RunNotFound,
                format!("unknown dag run \"{selected}\""),
            )
            .with_run_id(selected.clone())
        })?;
    if old.parent_session_id != params.parent_session_id {
        return Err(DagManagerError::new(
            DagManagerErrorCode::RunNotOwned,
            format!("dag run \"{selected}\" belongs to another session"),
        )
        .with_run_id(selected));
    }
    if old.status == DagRunStatus::Paused
        && old.pause.as_ref().map(|p| p.reason.as_str()) != Some("user_stop")
        && live_lease_holder(&old)
    {
        let mut error = DagManagerError::new(
            DagManagerErrorCode::InvalidAmendment,
            format!("dag run \"{selected}\" is paused by another live lease"),
        );
        error.run_id = Some(selected);
        return Err(error);
    }
    let at = crate::shared::iso_from_ms((inner.now)());
    let compiled = compile_dag(
        &params.definition,
        &DagCompileOptions {
            at: Some(at.clone()),
            settings: Some(inner.settings),
        },
    );
    if !compiled.ok {
        let mut error = DagManagerError::new(
            DagManagerErrorCode::InvalidDefinition,
            compiled
                .errors
                .iter()
                .map(|e| e.message.clone())
                .collect::<Vec<_>>()
                .join("; "),
        )
        .with_compile(compiled.errors, compiled.diagnostics);
        error.run_id = Some(selected.clone());
        return Err(error);
    }
    if params.definition.key != old.run_key {
        return Err(DagManagerError::new(
            DagManagerErrorCode::InvalidAmendment,
            "an amendment cannot change the run key",
        )
        .with_run_id(selected));
    }
    let old_inputs: HashMap<_, _> = old
        .definition
        .nodes
        .iter()
        .map(|n| (n.id.clone(), n))
        .collect();
    let new_ids: HashSet<_> = params
        .definition
        .nodes
        .iter()
        .map(|n| n.id.clone())
        .collect();
    let old_nodes: HashMap<_, _> = old.nodes.iter().map(|n| (n.id.clone(), n)).collect();
    let mut changed = Vec::new();
    let mut added = Vec::new();
    let mut removed = Vec::new();
    for node in &params.definition.nodes {
        match old_inputs.get(&node.id) {
            None => added.push(node.id.clone()),
            Some(previous)
                if previous.prompt != node.prompt
                    || previous.label != node.label
                    || previous.depends_on != node.depends_on
                    || previous.target != persisted_target(&node.target)
                    || previous.description != node.description
                    || previous.task_summary != node.task_summary =>
            {
                changed.push(node.id.clone())
            }
            _ => (),
        }
    }
    for node in &old.definition.nodes {
        if !new_ids.contains(&node.id) {
            removed.push(node.id.clone());
        }
    }
    let unsafe_ids: Vec<_> = changed
        .iter()
        .chain(removed.iter())
        .filter(|id| paused_amendment_unsafe(&old, id))
        .cloned()
        .collect();
    if !unsafe_ids.is_empty() {
        return Err(DagManagerError::new(
            DagManagerErrorCode::AmendRunningNode,
            format!(
                "cannot amend scheduled or running nodes: {}",
                unsafe_ids.join(", ")
            ),
        )
        .with_run_id(selected)
        .with_node_ids(unsafe_ids));
    }
    if old.status == DagRunStatus::Running {
        return Err(DagManagerError::new(
            DagManagerErrorCode::RunStillActive,
            format!("dag run \"{selected}\" is still active"),
        )
        .with_run_id(selected));
    }
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();
    for edge in &compiled.edges {
        dependents
            .entry(edge.from.clone())
            .or_default()
            .push(edge.to.clone());
    }
    let mut seen = HashSet::new();
    let mut invalidated_order = Vec::new();
    let mut queue: VecDeque<_> = changed.iter().chain(added.iter()).cloned().collect();
    while let Some(id) = queue.pop_front() {
        if seen.insert(id.clone()) {
            if !added.contains(&id) {
                invalidated_order.push(id.clone());
            }
            queue.extend(dependents.get(&id).into_iter().flatten().cloned());
        }
    }
    let invalidated = invalidated_order;
    let invalidated_set: HashSet<_> = invalidated.iter().cloned().collect();
    let mut nodes = compiled.nodes.clone();
    for node in &mut nodes {
        if let Some(previous) = old_nodes.get(&node.id) {
            if !invalidated_set.contains(&node.id) {
                *node = (*previous).clone();
            } else {
                node.exec_attempt = Some(previous.exec_attempt.unwrap_or(0) + 1);
                node.state = DagNodeState::Pending;
                node.task_id = previous.task_id.clone();
                node.attempt = previous.attempt;
                node.started_at = None;
                node.completed_at = None;
                node.error = None;
                node.run_stats = None;
                node.output = None;
                node.output_bytes = None;
            }
        }
    }
    let materialized = materialize_amended_definition(
        inner,
        &old,
        &params.definition,
        changed.iter().chain(added.iter()).cloned().collect(),
        &at,
    )?;
    let fingerprint = fingerprint_definition(&params.definition);
    let mut amended = old.clone();
    amended.name = params.definition.name.clone();
    amended.definition = materialized.definition;
    amended.definition_fingerprint = fingerprint.clone();
    amended.nodes = nodes;
    amended.edges = compiled.edges;
    amended.waves = compiled.waves;
    amended.critical_path = compiled.critical_path;
    amended.bottlenecks = compiled.bottlenecks;
    amended.diagnostics = materialized.diagnostics;
    amended.updated_at = at.clone();
    let mut history = amended.amend_history.clone().unwrap_or_default();
    history.push(AmendRecord {
        at,
        previous_fingerprint: old.definition_fingerprint.clone(),
        fingerprint: fingerprint.clone(),
        changed_node_ids: changed.clone(),
        added_node_ids: added.clone(),
        invalidated_node_ids: invalidated.clone(),
    });
    amended.amend_history = Some(history);
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&inner.store),
        run_id: selected.clone(),
        initial_checkpoint: old.clone(),
        apply_event: Arc::new(apply_run_event),
        subscriber_ring: None,
        now: Some(Arc::clone(&inner.now)),
    })
    .map_err(DagManagerError::from)?;
    journal.set_skip_duplicate(Arc::new(skip_duplicate_terminal_transition));
    journal
        .append(DagRunEventPayload::DefinitionAmended {
            previous_fingerprint: old.definition_fingerprint,
            fingerprint: fingerprint.clone(),
            changed_node_ids: changed,
            added_node_ids: added,
            invalidated_node_ids: invalidated,
            definition: amended.definition.clone(),
            nodes: amended.nodes.clone(),
            edges: amended.edges.clone(),
            waves: amended.waves.clone(),
            critical_path: amended.critical_path.clone(),
            bottlenecks: amended.bottlenecks.clone(),
            diagnostics: amended.diagnostics.clone(),
        })
        .map_err(DagManagerError::from)?;
    inner
        .store
        .write_key(&crate::dag::store::DagKeyRecord {
            schema_version: SchemaVersion1,
            parent_session_id: old.parent_session_id,
            run_key: old.run_key,
            run_id: selected.clone(),
            definition_fingerprint: Some(fingerprint.clone()),
        })
        .map_err(DagManagerError::from)?;
    let result = journal.snapshot();
    if result.definition_fingerprint != fingerprint {
        return Err(DagManagerError::new(
            DagManagerErrorCode::RunStillActive,
            format!("dag run \"{selected}\" changed before the amendment could be applied"),
        )
        .with_run_id(selected));
    }
    Ok(result)
}

fn materialize_amended_definition(
    inner: &DagManagerInner,
    old: &DagRunRecordV1,
    definition: &DagDefinition,
    rematerialized_ids: Vec<String>,
    at: &str,
) -> Result<MaterializedAmendment, DagManagerError> {
    let old_nodes: HashMap<_, _> = old
        .definition
        .nodes
        .iter()
        .map(|node| (node.id.as_str(), node))
        .collect();
    if inner.materialize_skills.is_none() || rematerialized_ids.is_empty() {
        return Ok(MaterializedAmendment {
            definition: DagPersistedDefinition {
                key: definition.key.clone(),
                name: definition.name.clone(),
                nodes: definition
                    .nodes
                    .iter()
                    .map(|node| {
                        let mut persisted = persisted_node(node, &HashMap::new());
                        persisted.effective_prompt = old_nodes.get(node.id.as_str()).map_or_else(
                            || node.prompt.clone(),
                            |old| old.effective_prompt.clone(),
                        );
                        persisted
                    })
                    .collect(),
            },
            diagnostics: old.diagnostics.clone(),
        });
    }
    let materializer = inner
        .materialize_skills
        .as_ref()
        .expect("checked materializer");
    let old_manifest = read_dag_skill_manifest(&inner.store, &old.run_id)?;
    let selected: Vec<_> = definition
        .nodes
        .iter()
        .filter(|node| rematerialized_ids.contains(&node.id))
        .cloned()
        .collect();
    let selected_definition = DagDefinition {
        key: definition.key.clone(),
        name: definition.name.clone(),
        nodes: selected.clone(),
    };
    let fresh = materializer(crate::dag::manager::DagMaterializeSkillsInput {
        run_id: &old.run_id,
        definition: &selected_definition,
        at,
    })?;
    if let Some(old_manifest) = old_manifest {
        let fresh_manifest =
            read_dag_skill_manifest(&inner.store, &old.run_id)?.unwrap_or(DagSkillManifest {
                schema_version: old_manifest.schema_version,
                run_id: old.run_id.clone(),
                cwd: old_manifest.cwd.clone(),
                at: at.to_string(),
                nodes: Vec::new(),
            });
        let replaced: HashSet<_> = selected.iter().map(|node| node.id.as_str()).collect();
        let mut manifest = fresh_manifest;
        manifest.cwd = old_manifest.cwd;
        manifest.nodes = old_manifest
            .nodes
            .into_iter()
            .filter(|node| !replaced.contains(node.node_id.as_str()))
            .chain(manifest.nodes)
            .collect();
        write_dag_skill_manifest(&inner.store, &manifest)?;
    }
    let fresh_prompts: HashMap<_, _> = fresh.nodes.into_iter().collect();
    let definition = DagPersistedDefinition {
        key: definition.key.clone(),
        name: definition.name.clone(),
        nodes: definition
            .nodes
            .iter()
            .map(|node| {
                let mut persisted = persisted_node(node, &HashMap::new());
                persisted.effective_prompt = fresh_prompts
                    .get(&node.id)
                    .cloned()
                    .or_else(|| {
                        old_nodes
                            .get(node.id.as_str())
                            .map(|old| old.effective_prompt.clone())
                    })
                    .unwrap_or_else(|| node.prompt.clone());
                persisted
            })
            .collect(),
    };
    let mut diagnostics = old.diagnostics.clone();
    diagnostics.extend(fresh.diagnostics);
    Ok(MaterializedAmendment {
        definition,
        diagnostics,
    })
}

struct MaterializedAmendment {
    definition: DagPersistedDefinition,
    diagnostics: Vec<DagDiagnostic>,
}

fn persisted_node(
    node: &DagNodeInput,
    effective_prompts: &std::collections::HashMap<String, String>,
) -> DagPersistedNode {
    let effective_prompt = effective_prompts
        .get(&node.id)
        .cloned()
        .unwrap_or_else(|| node.prompt.clone());
    DagPersistedNode {
        id: node.id.clone(),
        prompt: node.prompt.clone(),
        target: persisted_target(&node.target),
        label: node.label.clone(),
        depends_on: node.depends_on.clone(),
        task_summary: node.task_summary.clone(),
        description: node.description.clone(),
        load_skills: node.load_skills.clone(),
        effective_prompt,
    }
}

pub fn paused_amendment_unsafe(record: &DagRunRecordV1, node_id: &str) -> bool {
    let Some(node) = record.nodes.iter().find(|entry| entry.id == node_id) else {
        return false;
    };
    match node.state {
        DagNodeState::Running | DagNodeState::Paused => true,
        DagNodeState::Scheduled => {
            let is_user_stop = record
                .pause
                .as_ref()
                .is_some_and(|p| p.reason == "user_stop");
            !is_user_stop || node.task_id.is_some()
        }
        _ => false,
    }
}

pub fn live_lease_holder(record: &DagRunRecordV1) -> bool {
    record
        .lease_holder_pid
        .or(record.previous_lease_holder_pid)
        .is_some_and(process_is_alive)
}

pub fn apply_run_pause(record: &DagRunRecordV1, event: &DagRunEvent) -> Option<DagRunRecordV1> {
    match &event.payload {
        DagRunEventPayload::RunPaused { reason, pause_seq } => {
            if reason.as_deref() != Some("user_stop") {
                return Some(DagRunRecordV1 {
                    status: DagRunStatus::Paused,
                    updated_at: event.at.clone(),
                    ..record.clone()
                });
            }
            let pause_seq = pause_seq.expect("user_stop requires its pause sequence");
            Some(DagRunRecordV1 {
                status: DagRunStatus::Paused,
                pause: Some(crate::dag::types::DagRunPause {
                    reason: "user_stop".to_string(),
                    pause_seq,
                }),
                updated_at: event.at.clone(),
                nodes: record
                    .nodes
                    .iter()
                    .map(|node| {
                        if node.state == DagNodeState::Running {
                            DagNode {
                                state: DagNodeState::Paused,
                                ..node.clone()
                            }
                        } else {
                            node.clone()
                        }
                    })
                    .collect(),
                ..record.clone()
            })
        }
        DagRunEventPayload::RunResumed { generation } => {
            Some(DagRunRecordV1 {
                status: DagRunStatus::Running,
                pause: None,
                completed_at: None,
                retry_on_resume: None,
                generation: *generation,
                updated_at: event.at.clone(),
                nodes: record
                    .nodes
                    .iter()
                    .map(|node| {
                        if node.state == DagNodeState::Paused {
                            DagNode {
                                state: if node.task_id.is_none() {
                                    DagNodeState::Pending
                                } else {
                                    DagNodeState::Running
                                },
                                ..node.clone()
                            }
                        } else {
                            node.clone()
                        }
                    })
                    .collect(),
                ..record.clone()
            })
        }
        _ => None,
    }
}

pub fn apply_paused_dag_event(record: &DagRunRecordV1, event: &DagRunEvent) -> Option<DagRunRecordV1> {
    match &event.payload {
        DagRunEventPayload::GuidanceAdded { guidance }
        | DagRunEventPayload::GuidanceEdited { guidance }
        | DagRunEventPayload::GuidanceRemoved { guidance }
        | DagRunEventPayload::GuidanceDispatching { guidance }
        | DagRunEventPayload::GuidanceDelivered { guidance } => {
            let mut list: Vec<crate::dag::types::Guidance> = record
                .guidance
                .clone()
                .unwrap_or_default()
                .into_iter()
                .filter(|entry| entry.id != guidance.id)
                .collect();
            list.push(guidance.clone());
            Some(DagRunRecordV1 {
                updated_at: event.at.clone(),
                guidance: Some(list),
                ..record.clone()
            })
        }
        DagRunEventPayload::NodeCancelRequested { node_id } => {
            let mut list = record.cancel_requested_nodes.clone().unwrap_or_default();
            if !list.contains(node_id) {
                list.push(node_id.clone());
            }
            Some(DagRunRecordV1 {
                updated_at: event.at.clone(),
                cancel_requested_nodes: Some(list),
                ..record.clone()
            })
        }
        DagRunEventPayload::NodeRetryStaged { node_ids } => {
            let mut list = record.retry_on_resume.clone().unwrap_or_default();
            for id in node_ids {
                if !list.contains(id) {
                    list.push(id.clone());
                }
            }
            Some(DagRunRecordV1 {
                updated_at: event.at.clone(),
                retry_on_resume: Some(list),
                ..record.clone()
            })
        }
        DagRunEventPayload::NodePauseRecovered { node_id, child_seq } => {
            let mut acks = record.paused_child_acks.clone().unwrap_or_default();
            acks.insert(node_id.clone(), *child_seq);
            Some(DagRunRecordV1 {
                updated_at: event.at.clone(),
                paused_child_acks: Some(acks),
                ..record.clone()
            })
        }
        DagRunEventPayload::NodePauseLost { node_id, error } => {
            Some(DagRunRecordV1 {
                updated_at: event.at.clone(),
                nodes: record
                    .nodes
                    .iter()
                    .map(|node| {
                        if &node.id == node_id {
                            DagNode {
                                state: DagNodeState::Failed,
                                error: Some(error.clone()),
                                completed_at: Some(event.at.clone()),
                                ..node.clone()
                            }
                        } else {
                            node.clone()
                        }
                    })
                    .collect(),
                ..record.clone()
            })
        }
        _ => None,
    }
}

pub fn apply_dag_run_mutation(record: &DagRunRecordV1, event: &DagRunEvent) -> DagRunRecordV1 {
    if let Some(mutated) = apply_paused_dag_event(record, event) {
        return mutated;
    }
    if let Some(mutated) = apply_run_pause(record, event) {
        return mutated;
    }
    if let DagRunEventPayload::DefinitionAmended {
        previous_fingerprint,
        fingerprint,
        changed_node_ids,
        added_node_ids,
        invalidated_node_ids,
        definition,
        nodes,
        edges,
        waves,
        critical_path,
        bottlenecks,
        diagnostics,
    } = &event.payload
    {
        if record.definition_fingerprint != *previous_fingerprint
            || record.status == DagRunStatus::Running
            || (record.status == DagRunStatus::Paused
                && record.pause.as_ref().map(|p| p.reason.as_str()) != Some("user_stop")
                && live_lease_holder(record))
        {
            return record.clone();
        }
        let resulting_ids: HashSet<_> = nodes.iter().map(|node| node.id.as_str()).collect();
        let removed_ids: Vec<_> = record
            .nodes
            .iter()
            .filter(|node| !resulting_ids.contains(node.id.as_str()))
            .map(|node| node.id.as_str())
            .collect();
        if changed_node_ids
            .iter()
            .map(String::as_str)
            .chain(removed_ids.iter().copied())
            .any(|id| paused_amendment_unsafe(record, id))
        {
            return record.clone();
        }
        let mut history = record.amend_history.clone().unwrap_or_default();
        history.push(AmendRecord {
            at: event.at.clone(),
            previous_fingerprint: previous_fingerprint.clone(),
            fingerprint: fingerprint.clone(),
            changed_node_ids: changed_node_ids.clone(),
            added_node_ids: added_node_ids.clone(),
            invalidated_node_ids: invalidated_node_ids.clone(),
        });
        let invalidated: HashSet<_> = invalidated_node_ids.iter().collect();
        return DagRunRecordV1 {
            name: definition.name.clone(),
            definition: definition.clone(),
            definition_fingerprint: fingerprint.clone(),
            nodes: nodes
                .iter()
                .cloned()
                .map(|node| {
                    if invalidated.contains(&node.id) {
                        DagNode {
                            state: DagNodeState::Pending,
                            error: None,
                            completed_at: None,
                            started_at: None,
                            run_stats: None,
                            output: None,
                            output_bytes: None,
                            ..node
                        }
                    } else {
                        node
                    }
                })
                .collect(),
            edges: edges.clone(),
            waves: waves.clone(),
            critical_path: critical_path.clone(),
            bottlenecks: bottlenecks.clone(),
            diagnostics: diagnostics.clone(),
            amend_history: Some(history),
            updated_at: event.at.clone(),
            ..record.clone()
        };
    }
    if let DagRunEventPayload::NodeRetried {
        node_id,
        exec_attempt,
        ..
    } = &event.payload
    {
        return DagRunRecordV1 {
            nodes: record
                .nodes
                .iter()
                .map(|node| {
                    if node.id == *node_id {
                        DagNode {
                            state: DagNodeState::Pending,
                            exec_attempt: Some(*exec_attempt),
                            error: None,
                            completed_at: None,
                            started_at: None,
                            run_stats: None,
                            output: None,
                            output_bytes: None,
                            ..node.clone()
                        }
                    } else {
                        node.clone()
                    }
                })
                .collect(),
            updated_at: event.at.clone(),
            ..record.clone()
        };
    }
    if matches!(event.payload, DagRunEventPayload::RunCreated { .. }) {
        DagRunRecordV1 {
            updated_at: event.at.clone(),
            ..record.clone()
        }
    } else {
        record.clone()
    }
}

fn apply_run_event(record: &DagRunRecordV1, event: &DagRunEvent) -> DagRunRecordV1 {
    apply_dag_run_mutation(record, event)
}

fn skip_duplicate_terminal_transition(
    record: &DagRunRecordV1,
    payload: &DagRunEventPayload,
) -> bool {
    let DagRunEventPayload::NodeTransitioned { node_id, to, .. } = payload else {
        return false;
    };
    if !matches!(
        to,
        DagNodeState::Completed
            | DagNodeState::Failed
            | DagNodeState::Cancelled
            | DagNodeState::Skipped
    ) {
        return false;
    }
    record
        .nodes
        .iter()
        .any(|node| node.id == *node_id && node.state == *to)
}

#[cfg(unix)]
fn process_is_alive(pid: i64) -> bool {
    // Same answer as `kill -0 <pid>` (EPERM and ESRCH both read as not alive) via a syscall instead of a fork.
    i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

#[cfg(not(unix))]
fn process_is_alive(_pid: i64) -> bool {
    false
}

fn fingerprint_definition(definition: &DagDefinition) -> String {
    let nodes: Vec<DagNodeFingerprintInputV1> = definition
        .nodes
        .iter()
        .map(|node| DagNodeFingerprintInputV1 {
            node_id: node.id.clone(),
            label: node.label.clone().unwrap_or_else(|| node.id.clone()),
            depends_on: node.depends_on.clone().unwrap_or_default(),
            prompt: node.prompt.clone(),
            route: route_of(&node.target),
            task_summary: node.task_summary.clone(),
            description: node.description.clone(),
            child_name: node.id.clone(),
        })
        .collect();
    dag_definition_fingerprint(&DagDefinitionFingerprintInputV1 {
        name: definition.name.clone(),
        scheduler: SCHEDULER_FINGERPRINT_INPUT,
        nodes,
    })
}

fn route_of(target: &DagNodeTarget) -> DagRoute {
    crate::dag::graph::route_of(target)
}

fn project_snapshot_with_state_dir(
    record: DagRunRecordV1,
    state_dir: Option<&std::path::Path>,
) -> DagRunSnapshot {
    let nodes: Vec<DagNode> = record
        .nodes
        .iter()
        .cloned()
        .map(|mut node| {
            if let (Some(task_id), Some(state_dir)) = (&node.task_id, state_dir)
                && matches!(node.state, DagNodeState::Scheduled | DagNodeState::Running | DagNodeState::Paused)
            {
                node.last_activity_at =
                    crate::dag::node_activity::read_dag_node_activity_at(state_dir, task_id);
            }
            node
        })
        .collect();
    DagRunSnapshot {
        schema_version: SchemaVersion1,
        run_id: record.run_id,
        run_key: record.run_key,
        name: record.name,
        parent_session_id: record.parent_session_id,
        root_session_id: record.root_session_id,
        status: record.status,
        generation: record.generation,
        created_at: record.created_at,
        started_at: record.started_at,
        completed_at: record.completed_at,
        definition_fingerprint: record.definition_fingerprint,
        last_seq: record.checkpoint_seq,
        counts: crate::dag::handle::count_dag_nodes(&record.nodes),
        pause: record.pause,
        nodes,
        edges: record.edges,
        waves: record.waves,
        critical_path: record.critical_path,
        bottlenecks: record.bottlenecks,
        diagnostics: record.diagnostics,
        amend_history: record.amend_history,
        lease_holder_pid: record.lease_holder_pid,
    }
}

#[cfg(unix)]
fn file_inode(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn file_inode(_metadata: &fs::Metadata) -> u64 {
    0
}

fn count_nodes(nodes: &[DagNode]) -> DagNodeCounts {
    crate::dag::handle::count_dag_nodes(nodes)
}

trait WithRunId<T> {
    fn map_err_with_run_id(self, run_id: &DagRunId) -> Result<T, DagManagerError>;
}

impl<T> WithRunId<T> for Result<T, DagManagerError> {
    fn map_err_with_run_id(self, run_id: &DagRunId) -> Result<T, DagManagerError> {
        self.map_err(|error| error.with_run_id(run_id.clone()))
    }
}

fn resolve_limit(
    requested: Option<usize>,
    fallback: usize,
    max: usize,
) -> Result<usize, DagManagerError> {
    match requested {
        None => Ok(fallback),
        Some(0) => Err(DagManagerError::new(
            DagManagerErrorCode::InvalidArguments,
            "limit must be a positive integer, received 0".to_string(),
        )),
        Some(value) => Ok(value.min(max)),
    }
}

fn parse_date_ms(value: &str) -> i64 {
    chrono::DateTime::parse_from_rfc3339(value)
        .map(|at| at.timestamp_millis())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
