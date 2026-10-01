//! `dag/manager.ts`: run lifecycle - key idempotency, session ownership, and snapshot projection.
// allow: SIZE_OK - the run lifecycle surface keeps key idempotency, session ownership, and snapshot
// projection on one contract so callers cannot bypass a step.

use std::fs;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::dag::fingerprint::{
    DagDefinitionFingerprintInputV1, DagNodeFingerprintInputV1, DagSchedulerContract,
    dag_definition_fingerprint,
};
use crate::dag::graph::{DagCompileError, DagCompileOptions, DagDefinition, DagNodeInput, compile_dag};
use crate::dag::journal::{DagJournalCheckpoint, DagJournalOptions, create_dag_journal};
use crate::dag::store::{DagEventPage, DagEventReadOptions, DagFileStore, DagStoreError};
use crate::dag::types::{
    DAG_SETTINGS_DEFAULTS, DagBottleneck, DagDiagnostic, DagEdge, DagEventLane, DagNode,
    DagNodeCounts, DagNodeId, DagNodeTarget, DagRoute, DagRunEvent, DagRunEventPayload,
    DagRunEventType, DagRunId, DagRunSnapshot, DagRunStatus, DagSettings, DagWave, SchemaVersion1,
};

const LIST_DEFAULT_LIMIT: usize = 100;
const LIST_MAX_LIMIT: usize = 256;

const SCHEDULER_FINGERPRINT_INPUT: DagSchedulerContract = DagSchedulerContract {
    wave_admission: "strict-barrier",
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
}

impl DagManagerErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidDefinition => "invalid_definition",
            Self::DefinitionConflict => "definition_conflict",
            Self::RunNotFound => "run_not_found",
            Self::RunNotOwned => "run_not_owned",
            Self::InvalidArguments => "invalid_arguments",
        }
    }
}

/// Every `DagManager` rejection. `code` is the wire vocabulary the dag tool and RPC handlers
/// surface verbatim; `errors`/`diagnostics` carry compile detail for `invalid_definition`.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct DagManagerError {
    pub code: DagManagerErrorCode,
    pub message: String,
    pub run_id: Option<DagRunId>,
    pub errors: Vec<DagCompileError>,
    pub diagnostics: Vec<DagDiagnostic>,
}

impl DagManagerError {
    fn new(code: DagManagerErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            run_id: None,
            errors: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn with_run_id(mut self, run_id: DagRunId) -> Self {
        self.run_id = Some(run_id);
        self
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
        DagNodeTarget::SubagentType { subagent_type, model } => {
            DagPersistedNodeTarget::SubagentType {
                subagent_type: subagent_type.clone(),
                model: model.clone(),
            }
        }
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
            .map(project_snapshot)
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

pub type DagMaterializeSkills =
    Arc<dyn Fn(DagMaterializeSkillsInput<'_>) -> DagSkillMaterialization + Send + Sync>;

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

struct DagManagerInner {
    store: Arc<DagFileStore>,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    new_run_id: Arc<dyn Fn() -> DagRunId + Send + Sync>,
    settings: DagSettings,
    materialize_skills: Option<DagMaterializeSkills>,
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
    let now = options
        .now
        .unwrap_or_else(|| Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX)));
    let new_run_id = options
        .new_run_id
        .unwrap_or_else(|| Arc::new(|| format!("dag_{}", uuid_v4()) as DagRunId));
    let settings = DagSettings {
        max_nodes_per_run: options
            .settings
            .map_or(DAG_SETTINGS_DEFAULTS.max_nodes_per_run, |s| s.max_nodes_per_run),
        ..options.settings.unwrap_or(DAG_SETTINGS_DEFAULTS)
    };
    DagManager {
        inner: Arc::new(DagManagerInner {
            store: options.store,
            now,
            new_run_id,
            settings,
            materialize_skills: options.materialize_skills,
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
            .map(project_snapshot)
    }

    pub fn record(
        &self,
        run_id: &DagRunId,
        parent_session_id: &str,
    ) -> Result<DagRunRecordV1, DagManagerError> {
        self.inner.owned_record(run_id, parent_session_id)
    }

    pub fn list(&self, parent_session_id: &str, limit: Option<usize>) -> Result<Vec<DagRunSummary>, DagManagerError> {
        let limit = resolve_limit(limit, LIST_DEFAULT_LIMIT, LIST_MAX_LIMIT)?;
        let mut summaries: Vec<DagRunSummary> = Vec::new();
        let Ok(entries) = fs::read_dir(&self.inner.store.paths.runs) else {
            return Ok(summaries);
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().is_none_or(|ext| ext != "json") {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Ok(Some(record)) = self.inner.store.read_checkpoint::<DagRunRecordV1>(stem) else {
                continue;
            };
            if record.parent_session_id != parent_session_id {
                continue;
            }
            summaries.push(DagRunSummary {
                run_id: record.run_id,
                run_key: record.run_key,
                name: record.name,
                parent_session_id: record.parent_session_id,
                status: record.status,
                created_at: record.created_at,
                updated_at: record.updated_at,
                counts: count_nodes(&record.nodes),
            });
        }
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
        return Err(DagManagerError {
            code: DagManagerErrorCode::InvalidDefinition,
            message,
            run_id: None,
            errors: compiled.errors,
            diagnostics: compiled.diagnostics,
        });
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
    let existing = match existing_key {
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
        // Reuse never re-materializes skills: the run keeps its creation-time effective_prompt.
        return Ok(DagStartResult {
            reused: true,
            snapshot: project_snapshot(existing),
        });
    }

    let run_id = (inner.new_run_id)();
    let materialized = inner.materialize_skills.as_ref().map(|materialize| {
        materialize(DagMaterializeSkillsInput {
            run_id: &run_id,
            definition,
            at,
        })
    });
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
        snapshot: project_snapshot(journal.snapshot()),
    })
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

/// The manager owns only creation, so the sole journaled transition here is `dag.run.created`;
/// the scheduler extends the reducer for wave and node events.
fn apply_run_event(record: &DagRunRecordV1, event: &DagRunEvent) -> DagRunRecordV1 {
    if matches!(event.payload, DagRunEventPayload::RunCreated { .. }) {
        DagRunRecordV1 {
            updated_at: event.at.clone(),
            ..record.clone()
        }
    } else {
        record.clone()
    }
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

fn project_snapshot(record: DagRunRecordV1) -> DagRunSnapshot {
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
        counts: count_nodes(&record.nodes),
        nodes: record.nodes,
        edges: record.edges,
        waves: record.waves,
        critical_path: record.critical_path,
        bottlenecks: record.bottlenecks,
        diagnostics: record.diagnostics,
    }
}

fn count_nodes(nodes: &[DagNode]) -> DagNodeCounts {
    let mut counts = DagNodeCounts {
        total: nodes.len(),
        ..DagNodeCounts::default()
    };
    for node in nodes {
        match node.state {
            crate::dag::types::DagNodeState::Pending => counts.pending += 1,
            crate::dag::types::DagNodeState::Blocked => counts.blocked += 1,
            crate::dag::types::DagNodeState::Scheduled => counts.scheduled += 1,
            crate::dag::types::DagNodeState::Running => counts.running += 1,
            crate::dag::types::DagNodeState::Completed => counts.completed += 1,
            crate::dag::types::DagNodeState::Failed => counts.failed += 1,
            crate::dag::types::DagNodeState::Cancelled => counts.cancelled += 1,
            crate::dag::types::DagNodeState::Skipped => counts.skipped += 1,
        }
    }
    counts
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
