//! Port of senpi `packages/agent/src/execution/remote-hand-ledger.ts`.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use maho_ai::types::BoxFuture;
use serde_json::Value;

use crate::durable::store::StoreValue;

use super::ledger::{ExecutionLedger, LedgerGuard};
use super::ledger_types::{
    ExecutionActor, ExecutionAdmission, ExecutionCommand, ExecutionEntry, ExecutionEvent,
    ExecutionKey, ExecutionOutcome, ExecutionProgress, FrozenCall, LedgerRefusal, LedgerReply,
};
use super::remote_protocol::{
    RemoteHandMessage, RemoteInvoke, RemoteTerminal, RemoteTerminalReason, RemoteToolResult,
};

const GENERATION: u64 = 1;

pub struct HandLedgerOptions {
    pub ledger: Arc<dyn ExecutionLedger>,
    pub executor: String,
    pub issuer: String,
    pub workspace: String,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub lease_ms: i64,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum HandAdmission {
    Run,
    Reply { message: RemoteHandMessage },
    InFlight,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RecoveredMessage {
    pub lane: String,
    pub message: RemoteHandMessage,
}

pub type AdmitsFn = Arc<dyn Fn() -> bool + Send + Sync>;

pub trait HandLedger: Send + Sync {
    fn admit(
        &self,
        invoke: RemoteInvoke,
        admits: Option<AdmitsFn>,
    ) -> BoxFuture<'_, Result<HandAdmission, String>>;

    fn settle(
        &self,
        invoke: RemoteInvoke,
        result: RemoteToolResult,
    ) -> BoxFuture<'_, Result<RemoteHandMessage, String>>;

    fn ack(
        &self,
        execution_id: &str,
        admits: Option<AdmitsFn>,
    ) -> BoxFuture<'_, Result<(), String>>;

    fn recover(&self) -> BoxFuture<'_, Result<Vec<RecoveredMessage>, String>>;
}

fn outcome_of(entry: &ExecutionEntry) -> Option<RemoteHandMessage> {
    let execution_id = entry.command_id.clone();
    match &entry.progress {
        ExecutionProgress::Acked => Some(RemoteHandMessage::Refused {
            execution_id,
            reason: "result_expired".to_owned(),
        }),
        ExecutionProgress::Settled { outcome } => match outcome {
            ExecutionOutcome::Result { value, .. } => {
                let parsed: Option<RemoteToolResult> = match value {
                    Value::String(s) => serde_json::from_str(s).ok(),
                    other => serde_json::from_value(other.clone()).ok(),
                };
                match parsed {
                    Some(result) => Some(RemoteHandMessage::Settled {
                        execution_id,
                        result,
                    }),
                    None => Some(RemoteHandMessage::Refused {
                        execution_id,
                        reason: "corrupt_result".to_owned(),
                    }),
                }
            }
            ExecutionOutcome::Blocked { .. } => Some(RemoteHandMessage::Refused {
                execution_id,
                reason: "foreign_outcome".to_owned(),
            }),
            ExecutionOutcome::Cancelled {
                hooks_ran,
                uncertain,
            } => Some(RemoteHandMessage::Terminal(RemoteTerminal {
                execution_id,
                reason: RemoteTerminalReason::Cancelled,
                uncertain: *uncertain,
                hooks_ran: *hooks_ran,
            })),
            ExecutionOutcome::LeaseExpired {
                hooks_ran,
                uncertain,
            } => Some(RemoteHandMessage::Terminal(RemoteTerminal {
                execution_id,
                reason: RemoteTerminalReason::LeaseExpired,
                uncertain: *uncertain,
                hooks_ran: *hooks_ran,
            })),
            ExecutionOutcome::HandRestarted {
                hooks_ran,
                uncertain,
            } => Some(RemoteHandMessage::Terminal(RemoteTerminal {
                execution_id,
                reason: RemoteTerminalReason::HandRestarted,
                uncertain: *uncertain,
                hooks_ran: *hooks_ran,
            })),
            ExecutionOutcome::Uncertain {
                hooks_ran,
                uncertain,
            } => Some(RemoteHandMessage::Terminal(RemoteTerminal {
                execution_id,
                reason: RemoteTerminalReason::Uncertain,
                uncertain: *uncertain,
                hooks_ran: *hooks_ran,
            })),
        },
        _ => None,
    }
}

struct StoreHandLedger {
    ledger: Arc<dyn ExecutionLedger>,
    executor: String,
    issuer: String,
    workspace: String,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    lease_ms: i64,
    owned: Mutex<HashSet<String>>,
    recovered: tokio::sync::OnceCell<Result<(), String>>,
}

impl StoreHandLedger {
    fn key_of(&self, execution_id: &str) -> ExecutionKey {
        ExecutionKey {
            issuer: self.issuer.clone(),
            command_id: execution_id.to_owned(),
        }
    }

    async fn apply(
        &self,
        execution_id: &str,
        event: ExecutionEvent,
        actor: &str,
        guard: Option<LedgerGuard>,
    ) -> Result<LedgerReply, String> {
        self.ledger
            .apply(
                ExecutionCommand {
                    key: self.key_of(execution_id),
                    generation: GENERATION,
                    actor: ExecutionActor {
                        id: actor.to_owned(),
                        approved_workspace: None,
                    },
                    now: (self.now)(),
                    event,
                },
                guard,
            )
            .await
            .map_err(|e| e.to_string())
    }

    fn fenced_by(admits: Option<AdmitsFn>) -> Option<LedgerGuard> {
        admits.map(|f| -> LedgerGuard {
            Arc::new(move || {
                if f() {
                    None
                } else {
                    Some(LedgerRefusal::EpochFenced)
                }
            })
        })
    }

    fn reply_from(reply: &LedgerReply, execution_id: &str) -> RemoteHandMessage {
        match reply {
            LedgerReply::Refused { reason } => RemoteHandMessage::Refused {
                execution_id: execution_id.to_owned(),
                reason: reason.as_str().to_owned(),
            },
            LedgerReply::ResultExpired => RemoteHandMessage::Refused {
                execution_id: execution_id.to_owned(),
                reason: "result_expired".to_owned(),
            },
            LedgerReply::Applied { entry } | LedgerReply::Replayed { entry } => outcome_of(entry)
                .unwrap_or_else(|| RemoteHandMessage::Refused {
                    execution_id: execution_id.to_owned(),
                    reason: "not_settled".to_owned(),
                }),
        }
    }

    async fn admit_new(
        &self,
        invoke: &RemoteInvoke,
        digest: &str,
        guard: Option<LedgerGuard>,
    ) -> Result<LedgerReply, String> {
        let admitted_at = (self.now)();
        self.ledger
            .admit(
                ExecutionAdmission {
                    issuer: self.issuer.clone(),
                    command_id: invoke.execution_id.clone(),
                    executor: self.executor.clone(),
                    workspace: self.workspace.clone(),
                    scope: invoke.scope.id.clone(),
                    generation: GENERATION,
                    request_digest: digest.to_owned(),
                    admitted_at,
                    lease_until: admitted_at + self.lease_ms,
                    reconcilable: false,
                },
                guard,
            )
            .await
            .map_err(|e| e.to_string())
    }

    async fn fenced(&self, execution_id: &str) -> Result<HandAdmission, String> {
        if self
            .ledger
            .read(self.key_of(execution_id))
            .await
            .map_err(|e| e.to_string())?
            .is_some()
        {
            let _ = self
                .apply(execution_id, ExecutionEvent::Cancel, &self.issuer, None)
                .await?;
        }
        Ok(HandAdmission::Reply {
            message: RemoteHandMessage::Refused {
                execution_id: execution_id.to_owned(),
                reason: "epoch_fenced".to_owned(),
            },
        })
    }

    fn is_fenced(reply: &LedgerReply) -> bool {
        matches!(
            reply,
            LedgerReply::Refused {
                reason: LedgerRefusal::EpochFenced
            }
        )
    }

    async fn advance(
        &self,
        invoke: &RemoteInvoke,
        guard: Option<LedgerGuard>,
    ) -> Result<HandAdmission, String> {
        let execution_id = &invoke.execution_id;
        let args: StoreValue = invoke.arguments.clone();
        let digest = serde_json::to_string(&(&invoke.name, &args)).unwrap_or_default();
        let frozen = FrozenCall {
            name: invoke.name.clone(),
            args,
            digest: digest.clone(),
        };
        let prior = self
            .ledger
            .read(self.key_of(execution_id))
            .await
            .map_err(|e| e.to_string())?;
        if let Some(ref p) = prior
            && (p.request_digest != digest || p.scope != invoke.scope.id)
        {
            return Ok(HandAdmission::Reply {
                message: RemoteHandMessage::Refused {
                    execution_id: execution_id.clone(),
                    reason: "identity_conflict".to_owned(),
                },
            });
        }
        if let Some(ref p) = prior
            && let Some(settled) = outcome_of(p)
        {
            return Ok(HandAdmission::Reply { message: settled });
        }

        let need_from_start = match prior.as_ref().map(|p| &p.progress) {
            None => true,
            Some(ExecutionProgress::Prepared { .. } | ExecutionProgress::Released { .. }) => false,
            Some(_) => return Ok(HandAdmission::InFlight),
        };

        if need_from_start {
            let steps = [
                None,
                Some(ExecutionEvent::Prepare),
                Some(ExecutionEvent::Effect),
                Some(ExecutionEvent::Freeze {
                    call: frozen,
                    awaiting_consent: false,
                }),
            ];
            for step in steps {
                let reply = match step {
                    None => self.admit_new(invoke, &digest, guard.clone()).await?,
                    Some(event) => {
                        self.apply(execution_id, event, &self.executor, guard.clone())
                            .await?
                    }
                };
                if Self::is_fenced(&reply) {
                    return self.fenced(execution_id).await;
                }
                match &reply {
                    LedgerReply::Replayed { entry }
                        if !matches!(entry.progress, ExecutionProgress::Settled { .. }) => {}
                    LedgerReply::Applied { entry }
                        if !matches!(entry.progress, ExecutionProgress::Settled { .. }) => {}
                    _ => {
                        return Ok(HandAdmission::Reply {
                            message: Self::reply_from(&reply, execution_id),
                        });
                    }
                }
            }
        }

        let release_reply = self
            .apply(
                execution_id,
                ExecutionEvent::Release,
                &self.issuer,
                guard.clone(),
            )
            .await?;
        if Self::is_fenced(&release_reply) {
            return self.fenced(execution_id).await;
        }
        match &release_reply {
            LedgerReply::Replayed { entry }
                if !matches!(entry.progress, ExecutionProgress::Settled { .. }) => {}
            LedgerReply::Applied { entry }
                if !matches!(entry.progress, ExecutionProgress::Settled { .. }) => {}
            _ => {
                return Ok(HandAdmission::Reply {
                    message: Self::reply_from(&release_reply, execution_id),
                });
            }
        }

        let dispatched = self
            .apply(
                execution_id,
                ExecutionEvent::Dispatch,
                &self.executor,
                guard,
            )
            .await?;
        if Self::is_fenced(&dispatched) {
            return self.fenced(execution_id).await;
        }
        match &dispatched {
            LedgerReply::Applied { entry }
                if matches!(entry.progress, ExecutionProgress::Dispatched { .. }) =>
            {
                Ok(HandAdmission::Run)
            }
            LedgerReply::Replayed { entry }
                if !matches!(entry.progress, ExecutionProgress::Settled { .. }) =>
            {
                Ok(HandAdmission::InFlight)
            }
            _ => Ok(HandAdmission::Reply {
                message: Self::reply_from(&dispatched, execution_id),
            }),
        }
    }
}

impl HandLedger for StoreHandLedger {
    fn admit(
        &self,
        invoke: RemoteInvoke,
        admits: Option<AdmitsFn>,
    ) -> BoxFuture<'_, Result<HandAdmission, String>> {
        Box::pin(async move {
            let execution_id = invoke.execution_id.clone();
            {
                let mut owned = self.owned.lock().unwrap_or_else(|p| p.into_inner());
                if owned.contains(&execution_id) {
                    return Ok(HandAdmission::InFlight);
                }
                owned.insert(execution_id.clone());
            }
            let res = self.advance(&invoke, Self::fenced_by(admits)).await;
            match &res {
                Ok(HandAdmission::Run) => {}
                _ => {
                    self.owned
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&execution_id);
                }
            }
            res
        })
    }

    fn settle(
        &self,
        invoke: RemoteInvoke,
        result: RemoteToolResult,
    ) -> BoxFuture<'_, Result<RemoteHandMessage, String>> {
        Box::pin(async move {
            let val = Value::String(serde_json::to_string(&result).unwrap_or_default());
            let res = self
                .apply(
                    &invoke.execution_id,
                    ExecutionEvent::Result { value: val },
                    &self.executor,
                    None,
                )
                .await;
            self.owned
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&invoke.execution_id);
            let reply = res?;
            Ok(Self::reply_from(&reply, &invoke.execution_id))
        })
    }

    fn ack(
        &self,
        execution_id: &str,
        admits: Option<AdmitsFn>,
    ) -> BoxFuture<'_, Result<(), String>> {
        let execution_id = execution_id.to_owned();
        Box::pin(async move {
            let _ = self
                .apply(
                    &execution_id,
                    ExecutionEvent::Ack,
                    &self.issuer,
                    Self::fenced_by(admits),
                )
                .await?;
            Ok(())
        })
    }

    fn recover(&self) -> BoxFuture<'_, Result<Vec<RecoveredMessage>, String>> {
        Box::pin(async move {
            let first = !self.recovered.initialized();
            let res = self
                .recovered
                .get_or_init(|| async {
                    self.ledger
                        .recover((self.now)())
                        .await
                        .map_err(|e| e.to_string())
                })
                .await
                .clone();
            res?;
            if !first {
                return Ok(Vec::new());
            }
            let entries = self.ledger.scan().await.map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            for entry in entries {
                if matches!(entry.progress, ExecutionProgress::Settled { .. })
                    && let Some(message) = outcome_of(&entry)
                {
                    out.push(RecoveredMessage {
                        lane: entry.scope,
                        message,
                    });
                }
            }
            Ok(out)
        })
    }
}

pub fn create_hand_ledger(options: HandLedgerOptions) -> Arc<dyn HandLedger> {
    Arc::new(StoreHandLedger {
        ledger: options.ledger,
        executor: options.executor,
        issuer: options.issuer,
        workspace: options.workspace,
        now: options.now,
        lease_ms: options.lease_ms,
        owned: Mutex::new(HashSet::new()),
        recovered: tokio::sync::OnceCell::new(),
    })
}
