use crate::abort::AbortSignal;
use crate::lsp::workspace_apply_edit_failure::WorkspaceApplyEditConcurrentPhase;
use crate::lsp::workspace_apply_edit_failure::workspace_apply_edit_concurrent_failure_reason;
use crate::lsp::workspace_document_state::WorkspaceDocumentState;
use crate::lsp::workspace_edit::ApplyResult;
use crate::lsp::workspace_edit::WorkspaceEditCommitIo;
use crate::lsp::workspace_edit::WorkspaceEditFingerprintResult;
use crate::lsp::workspace_edit::WorkspaceEditPlanResult;
use crate::lsp::workspace_edit::commit_workspace_edit_plan;
use crate::lsp::workspace_edit::fingerprint_workspace_edit;
use crate::lsp::workspace_edit::plan_workspace_edit;
use serde::Serialize;
use serde_json::Value;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::PoisonError;
use tokio::sync::watch;

/// TS `WorkspaceApplyEditResponse` (the `workspace/applyEdit` result).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceApplyEditResponse {
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_change: Option<usize>,
}

/// TS `WorkspaceMutationLease`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkspaceMutationLease {
    pub id: u64,
}

/// TS `LspRenameResult`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LspRenameResult {
    pub edit: Option<Value>,
    pub apply: ApplyResult,
}

/// TS `AcquireMutationLeaseResult`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquireMutationLeaseResult {
    Success(WorkspaceMutationLease),
    Failure(ApplyResult),
}

#[derive(Debug, Clone)]
struct ServerApplyRecord {
    fingerprint: Option<String>,
    result: ApplyResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LeasePhase {
    Idle,
    Applying,
    Settled,
    Sealed,
}

#[derive(Debug)]
struct ActiveLease {
    id: u64,
    signal: Option<AbortSignal>,
    phase: LeasePhase,
    server_apply: Option<ServerApplyRecord>,
    apply_done: watch::Sender<bool>,
}

#[derive(Debug, Default)]
struct ControllerState {
    active: Option<ActiveLease>,
    next_lease_id: u64,
}

fn failure(
    message: impl Into<String>,
    failed_change: Option<usize>,
    base: Option<&ApplyResult>,
) -> ApplyResult {
    ApplyResult {
        success: false,
        files_modified: base
            .map(|base| base.files_modified.clone())
            .unwrap_or_default(),
        total_edits: base.map_or(0, |base| base.total_edits),
        errors: vec![message.into()],
        failed_change,
        late_abort: base.and_then(|base| base.late_abort.filter(|late| *late)),
    }
}

fn response_for(result: &ApplyResult) -> WorkspaceApplyEditResponse {
    if result.success {
        return WorkspaceApplyEditResponse {
            applied: true,
            failure_reason: None,
            failed_change: None,
        };
    }
    WorkspaceApplyEditResponse {
        applied: false,
        failure_reason: Some(
            result
                .errors
                .first()
                .cloned()
                .unwrap_or_else(|| "workspace edit failed".to_string()),
        ),
        failed_change: result.failed_change,
    }
}

/// TS `WorkspaceMutationController`: scopes server `workspace/applyEdit` requests to one
/// active mutating request and reconciles them with the rename response.
pub struct WorkspaceMutationController {
    workspace_root: String,
    documents: Arc<WorkspaceDocumentState>,
    state: Mutex<ControllerState>,
    io: Mutex<WorkspaceEditCommitIo>,
}

impl std::fmt::Debug for WorkspaceMutationController {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WorkspaceMutationController")
            .field("workspace_root", &self.workspace_root)
            .finish()
    }
}

impl WorkspaceMutationController {
    pub fn new(workspace_root: impl Into<String>, documents: Arc<WorkspaceDocumentState>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            documents,
            state: Mutex::new(ControllerState {
                active: None,
                next_lease_id: 1,
            }),
            io: Mutex::new(WorkspaceEditCommitIo::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ControllerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn set_io(&self, io: WorkspaceEditCommitIo) {
        *self.io.lock().unwrap_or_else(PoisonError::into_inner) = io;
    }

    pub fn acquire(&self, signal: Option<&AbortSignal>) -> AcquireMutationLeaseResult {
        let mut state = self.lock();
        if state.active.is_some() {
            return AcquireMutationLeaseResult::Failure(failure(
                "workspace mutation is already in progress",
                None,
                None,
            ));
        }
        if signal.is_some_and(AbortSignal::aborted) {
            return AcquireMutationLeaseResult::Failure(failure(
                "cancelled before mutating request",
                None,
                None,
            ));
        }
        let id = state.next_lease_id;
        state.next_lease_id += 1;
        state.active = Some(ActiveLease {
            id,
            signal: signal.cloned(),
            phase: LeasePhase::Idle,
            server_apply: None,
            apply_done: watch::channel(false).0,
        });
        AcquireMutationLeaseResult::Success(WorkspaceMutationLease { id })
    }

    pub fn release(&self, lease: WorkspaceMutationLease) {
        let mut state = self.lock();
        if state
            .active
            .as_ref()
            .is_some_and(|active| active.id == lease.id)
        {
            state.active = None;
        }
    }

    pub fn is_before_commit(&self, lease: WorkspaceMutationLease) -> bool {
        self.lock()
            .active
            .as_ref()
            .is_some_and(|active| active.id == lease.id && active.phase == LeasePhase::Idle)
    }

    /// TS `handleApplyEdit`: the server-initiated `workspace/applyEdit` handler.
    pub async fn handle_apply_edit(&self, params: &Value) -> WorkspaceApplyEditResponse {
        let (lease_id, signal) = {
            let mut state = self.lock();
            let Some(lease) = state.active.as_mut() else {
                return WorkspaceApplyEditResponse {
                    applied: false,
                    failure_reason: Some(
                        "workspace/applyEdit requires an active workspace mutation".to_string(),
                    ),
                    failed_change: None,
                };
            };
            if lease.phase != LeasePhase::Idle {
                let phase = if lease.phase == LeasePhase::Applying {
                    WorkspaceApplyEditConcurrentPhase::Applying
                } else {
                    WorkspaceApplyEditConcurrentPhase::Settled
                };
                return WorkspaceApplyEditResponse {
                    applied: false,
                    failure_reason: Some(
                        workspace_apply_edit_concurrent_failure_reason(phase).to_string(),
                    ),
                    failed_change: None,
                };
            }
            lease.phase = LeasePhase::Applying;
            (lease.id, lease.signal.clone())
        };
        let edit = params.as_object().and_then(|params| params.get("edit"));
        let record = match edit {
            None => ServerApplyRecord {
                fingerprint: None,
                result: failure("workspace/applyEdit params.edit is required", Some(0), None),
            },
            Some(edit) => self.apply_edit(edit, signal.as_ref()).await,
        };
        let response = response_for(&record.result);
        let mut state = self.lock();
        if let Some(lease) = state.active.as_mut().filter(|lease| lease.id == lease_id) {
            lease.server_apply = Some(record);
            lease.phase = LeasePhase::Settled;
            lease.apply_done.send_replace(true);
        }
        response
    }

    /// TS `reconcileRename`: waits for an in-flight server apply, then either verifies the
    /// rename response matches it or applies the rename edit itself.
    pub async fn reconcile_rename(
        &self,
        lease: WorkspaceMutationLease,
        edit: Option<Value>,
    ) -> LspRenameResult {
        let mut apply_done = {
            let state = self.lock();
            let Some(active) = state.active.as_ref().filter(|active| active.id == lease.id) else {
                return LspRenameResult {
                    edit,
                    apply: failure(
                        "workspace mutation lease ended before rename reconciliation",
                        None,
                        None,
                    ),
                };
            };
            (active.phase == LeasePhase::Applying).then(|| active.apply_done.subscribe())
        };
        if let Some(receiver) = apply_done.as_mut() {
            let _ = receiver.wait_for(|done| *done).await;
        }
        let (server_apply, signal) = {
            let mut state = self.lock();
            let Some(active) = state.active.as_mut().filter(|active| active.id == lease.id) else {
                return LspRenameResult {
                    edit,
                    apply: failure(
                        "workspace mutation lease ended before rename reconciliation",
                        None,
                        None,
                    ),
                };
            };
            if active.server_apply.is_none() {
                active.phase = LeasePhase::Sealed;
            }
            (active.server_apply.clone(), active.signal.clone())
        };
        if let Some(record) = server_apply {
            return self.reconcile_server_apply(record, edit);
        }
        let Some(rename_edit) = edit.as_ref() else {
            return LspRenameResult {
                edit,
                apply: failure("No edit provided", None, None),
            };
        };
        let applied = self.apply_edit(rename_edit, signal.as_ref()).await;
        LspRenameResult {
            edit,
            apply: applied.result,
        }
    }

    fn reconcile_server_apply(
        &self,
        record: ServerApplyRecord,
        edit: Option<Value>,
    ) -> LspRenameResult {
        let Some(rename_edit) = edit.as_ref() else {
            return LspRenameResult {
                edit,
                apply: record.result,
            };
        };
        let matches = match fingerprint_workspace_edit(rename_edit, &self.workspace_root) {
            WorkspaceEditFingerprintResult::Success(fingerprint) => {
                record.fingerprint.as_ref() == Some(&fingerprint)
            }
            WorkspaceEditFingerprintResult::Failure(_) => false,
        };
        if matches {
            return LspRenameResult {
                edit,
                apply: record.result,
            };
        }
        LspRenameResult {
            edit,
            apply: failure(
                "rename result conflicts with server-applied workspace edit",
                Some(0),
                Some(&record.result),
            ),
        }
    }

    async fn apply_edit(&self, edit: &Value, signal: Option<&AbortSignal>) -> ServerApplyRecord {
        let plan = match plan_workspace_edit(edit, &self.workspace_root) {
            WorkspaceEditPlanResult::Success(plan) => plan,
            WorkspaceEditPlanResult::Failure(result) => {
                return ServerApplyRecord {
                    fingerprint: None,
                    result,
                };
            }
        };
        let fingerprint = Some(plan.fingerprint.clone());
        if let Some(version_failure) = self.documents.validate_versions(&plan.operations) {
            return ServerApplyRecord {
                fingerprint,
                result: failure(
                    version_failure.message,
                    Some(version_failure.change_index),
                    None,
                ),
            };
        }
        let commit = {
            let mut io = self.io.lock().unwrap_or_else(PoisonError::into_inner);
            commit_workspace_edit_plan(&plan, signal, &mut io)
        };
        let mut result = commit.result;
        if !commit.delta.operations.is_empty()
            && let Err(error) = self.documents.synchronize(&commit.delta).await
        {
            result = failure(
                format!("document synchronization failed after filesystem commit: {error}"),
                None,
                Some(&result),
            );
        }
        if signal.is_some_and(AbortSignal::aborted) && result.late_abort != Some(true) {
            result.late_abort = Some(true);
        }
        ServerApplyRecord {
            fingerprint,
            result,
        }
    }
}
