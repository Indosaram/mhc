//! Port of senpi `packages/agent/src/execution/ledger.ts`.

use std::sync::Arc;

use maho_ai::types::BoxFuture;
use serde_json::json;

use crate::durable::store::{DurableStore, DurableStoreError, StoreWrite};
use crate::durable::store_admission::AdmissionLine;

use super::ledger_codec::decode_execution;
use super::ledger_transition::transition_execution;
use super::ledger_types::{
    ExecutionAdmission, ExecutionCommand, ExecutionEntry, ExecutionEvent, ExecutionKey,
    ExecutionProgress, LedgerRefusal, LedgerReply, MAX_EXECUTION_LIFETIME_MS, applied, refused,
    replayed,
};

const PREFIX: &str = "execution.ledger:";

fn key_of(key: &ExecutionKey) -> String {
    format!(
        "{PREFIX}{}",
        serde_json::to_string(&[&key.issuer, &key.command_id]).unwrap_or_default()
    )
}

/// A precondition checked inside a mutation's admission slot, before anything is read or written.
pub type LedgerGuard = Arc<dyn Fn() -> Option<LedgerRefusal> + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LedgerError {
    #[error(transparent)]
    Store(#[from] DurableStoreError),
    #[error("{0}")]
    Other(String),
}

impl From<LedgerError> for DurableStoreError {
    fn from(err: LedgerError) -> Self {
        match err {
            LedgerError::Store(store) => store,
            LedgerError::Other(msg) => DurableStoreError::Other(msg),
        }
    }
}

pub trait ExecutionLedger: Send + Sync {
    fn admit(
        &self,
        input: ExecutionAdmission,
        guard: Option<LedgerGuard>,
    ) -> BoxFuture<'_, Result<LedgerReply, LedgerError>>;

    fn apply(
        &self,
        command: ExecutionCommand,
        guard: Option<LedgerGuard>,
    ) -> BoxFuture<'_, Result<LedgerReply, LedgerError>>;

    fn read(&self, key: ExecutionKey)
    -> BoxFuture<'_, Result<Option<ExecutionEntry>, LedgerError>>;

    /// Called once on endpoint restart; also expires overdue leases and reconciliation deadlines.
    fn recover(&self, now: i64) -> BoxFuture<'_, Result<(), LedgerError>>;

    /// Every execution row this ledger holds, in key order.
    fn scan(&self) -> BoxFuture<'_, Result<Vec<ExecutionEntry>, LedgerError>>;
}

pub struct StoreExecutionLedgerOptions {
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
}

#[derive(Clone)]
pub struct StoreExecutionLedger {
    store: Arc<dyn DurableStore>,
    epoch: u64,
    admission: AdmissionLine,
}

impl StoreExecutionLedger {
    async fn read_inner(&self, key: &ExecutionKey) -> Result<Option<ExecutionEntry>, LedgerError> {
        let value = self.store.get_document(&key_of(key)).await?;
        match value {
            None => Ok(None),
            Some(v) => decode_execution(&v)
                .map(Some)
                .map_err(|e| LedgerError::Other(e.to_string())),
        }
    }

    async fn persist(
        &self,
        entry: &ExecutionEntry,
        event: &str,
        audit: bool,
    ) -> Result<(), LedgerError> {
        let mut writes = vec![StoreWrite::Append {
            id: self.store.mint_id(),
            kind: if audit {
                "execution.audit".to_owned()
            } else {
                "execution.transition".to_owned()
            },
            data: json!({
                "issuer": entry.issuer,
                "commandId": entry.command_id,
                "generation": entry.generation,
                "event": event,
                "state": entry.progress.state_str(),
            }),
        }];
        if !audit {
            let data =
                serde_json::to_value(entry).map_err(|e| LedgerError::Other(e.to_string()))?;
            writes.push(StoreWrite::Put {
                key: key_of(&entry.key()),
                data,
            });
        }
        self.store
            .commit(self.epoch, writes, Box::new(|_| {}))
            .await
            .map(|_| ())
            .map_err(LedgerError::Store)
    }

    async fn apply_inner(
        &self,
        command: ExecutionCommand,
        guard: Option<LedgerGuard>,
    ) -> Result<LedgerReply, LedgerError> {
        let ledger = self.clone();
        self.admission
            .admit(|| async move {
                if let Some(guard) = guard
                    && let Some(refusal) = guard()
                {
                    return Ok(refused(refusal));
                }
                let Some(entry) = ledger.read_inner(&command.key).await? else {
                    return Ok(refused(LedgerRefusal::NotFound));
                };
                let reply = transition_execution(entry.clone(), &command);
                match &reply {
                    LedgerReply::Applied { entry: updated } => {
                        ledger
                            .persist(updated, command.event.kind_str(), false)
                            .await?;
                    }
                    LedgerReply::Replayed { .. }
                        if matches!(entry.progress, ExecutionProgress::Settled { .. })
                            && matches!(
                                command.event,
                                ExecutionEvent::Result { .. } | ExecutionEvent::Cancel
                            ) =>
                    {
                        ledger
                            .persist(&entry, command.event.kind_str(), true)
                            .await?;
                    }
                    _ => {}
                }
                Ok(reply)
            })
            .await
    }
}

impl ExecutionLedger for StoreExecutionLedger {
    fn admit(
        &self,
        input: ExecutionAdmission,
        guard: Option<LedgerGuard>,
    ) -> BoxFuture<'_, Result<LedgerReply, LedgerError>> {
        let ledger = self.clone();
        Box::pin(async move {
            let admission = ledger.admission.clone();
            admission
                .admit(|| async move {
                    if let Some(guard) = guard
                        && let Some(refusal) = guard()
                    {
                        return Ok(refused(refusal));
                    }
                    if let Some(prior) = ledger.read_inner(&input.key()).await? {
                        let same = prior.executor == input.executor
                            && prior.workspace == input.workspace
                            && prior.scope == input.scope
                            && prior.generation == input.generation
                            && prior.request_digest == input.request_digest
                            && prior.admitted_at == input.admitted_at
                            && prior.initial_lease_until == input.lease_until
                            && prior.reconcilable == input.reconcilable;
                        return Ok(if same {
                            replayed(prior)
                        } else {
                            refused(LedgerRefusal::IdentityConflict)
                        });
                    }
                    let valid_strings = [
                        &input.issuer,
                        &input.command_id,
                        &input.executor,
                        &input.workspace,
                        &input.scope,
                        &input.request_digest,
                    ]
                    .iter()
                    .all(|s| !s.is_empty());
                    if !valid_strings
                        || input.generation < 1
                        || input.lease_until <= input.admitted_at
                        || input.lease_until > input.admitted_at + MAX_EXECUTION_LIFETIME_MS
                    {
                        return Ok(refused(LedgerRefusal::InvalidAdmission));
                    }
                    let entry = ExecutionEntry {
                        issuer: input.issuer,
                        command_id: input.command_id,
                        executor: input.executor,
                        workspace: input.workspace,
                        scope: input.scope,
                        generation: input.generation,
                        request_digest: input.request_digest,
                        admitted_at: input.admitted_at,
                        lease_until: input.lease_until,
                        reconcilable: input.reconcilable,
                        initial_lease_until: input.lease_until,
                        renew_seq: 0,
                        effect_possible: false,
                        body_dispatched: false,
                        progress: ExecutionProgress::Admitted,
                    };
                    ledger.persist(&entry, "admit", false).await?;
                    Ok(applied(entry))
                })
                .await
        })
    }

    fn apply(
        &self,
        command: ExecutionCommand,
        guard: Option<LedgerGuard>,
    ) -> BoxFuture<'_, Result<LedgerReply, LedgerError>> {
        Box::pin(self.apply_inner(command, guard))
    }

    fn read(
        &self,
        key: ExecutionKey,
    ) -> BoxFuture<'_, Result<Option<ExecutionEntry>, LedgerError>> {
        Box::pin(async move { self.read_inner(&key).await })
    }

    fn recover(&self, now: i64) -> BoxFuture<'_, Result<(), LedgerError>> {
        Box::pin(async move {
            let docs = self.store.scan_documents(PREFIX).await?;
            for doc in docs {
                let entry =
                    decode_execution(&doc.data).map_err(|e| LedgerError::Other(e.to_string()))?;
                let cmd = ExecutionCommand {
                    key: entry.key(),
                    generation: entry.generation,
                    actor: super::ledger_types::ExecutionActor {
                        id: entry.executor.clone(),
                        approved_workspace: None,
                    },
                    now,
                    event: ExecutionEvent::Restart,
                };
                self.apply_inner(cmd, None).await?;
            }
            Ok(())
        })
    }

    fn scan(&self) -> BoxFuture<'_, Result<Vec<ExecutionEntry>, LedgerError>> {
        Box::pin(async move {
            let docs = self.store.scan_documents(PREFIX).await?;
            docs.iter()
                .map(|doc| {
                    decode_execution(&doc.data).map_err(|e| LedgerError::Other(e.to_string()))
                })
                .collect()
        })
    }
}

pub fn create_execution_ledger(options: StoreExecutionLedgerOptions) -> Arc<dyn ExecutionLedger> {
    Arc::new(StoreExecutionLedger {
        store: options.store,
        epoch: options.epoch,
        admission: options.admission,
    })
}
