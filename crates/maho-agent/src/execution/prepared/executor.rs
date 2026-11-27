//! Port of senpi `packages/agent/src/execution/prepared/executor.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use maho_ai::types::BoxFuture;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::agent_loop::prepare_agent_tool_call;
use crate::execution::canonical_digest::canonical_digest;
use crate::execution::ledger::ExecutionLedger;
use crate::execution::ledger_types::{
    ExecutionActor, ExecutionAdmission, ExecutionCommand, ExecutionEntry, ExecutionEvent,
    ExecutionKey, ExecutionOutcome, ExecutionProgress, LedgerRefusal, LedgerReply,
};
use crate::invocation_input::{InvocationInput, invoke_with_input};
use crate::types::BeforeToolCallContext;

use super::executor_body::{PreparedBody, error_result, finalize_body, run_body};
use super::executor_freeze::{
    StoppedReply, freeze_call, recorded_result, stopped_reply, stopped_value,
};
use super::executor_types::{
    ConsentClassifier, ConsentReply, ConsentRequest, PreparationPipeline, PrepareReply,
    PrepareRequest, PreparedExecutor, PreparedExecutorOptions, ReleaseReply, ReleaseRequest,
};

fn default_clock() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn key_id(key: &ExecutionKey) -> String {
    serde_json::to_string(&[&key.issuer, &key.command_id]).unwrap_or_default()
}

fn key_of(request: &PrepareRequest) -> ExecutionKey {
    ExecutionKey {
        issuer: request.issuer.clone(),
        command_id: request
            .command_id
            .clone()
            .unwrap_or_else(|| request.call.id.clone()),
    }
}

fn refusal_from_reply(reply: &LedgerReply) -> String {
    match reply {
        LedgerReply::Refused { reason } => reason.as_str().to_owned(),
        LedgerReply::Applied { .. } => "applied".to_owned(),
        LedgerReply::Replayed { .. } => "replayed".to_owned(),
        LedgerReply::ResultExpired => "result_expired".to_owned(),
    }
}

fn incomplete_message(name: &str, error_message: Option<&str>) -> String {
    if let Some(msg) = error_message {
        let suffix = if msg.ends_with('.') { "" } else { "." };
        format!("{msg}{suffix} Re-issue the tool call with complete arguments.")
    } else {
        format!(
            "Tool call \"{name}\" was not executed: the response ended before the tool call was complete because it hit the output token limit. Re-issue the tool call with complete arguments."
        )
    }
}

fn from_row(entry: &ExecutionEntry) -> PrepareReply {
    match &entry.progress {
        ExecutionProgress::PreparedAwaitingConsent { call } => {
            PrepareReply::PreparedAwaitingConsent {
                call: call.clone(),
                requested_name: None,
            }
        }
        ExecutionProgress::Prepared { call }
        | ExecutionProgress::Released { call }
        | ExecutionProgress::Dispatched { call }
        | ExecutionProgress::Reconciling { call, .. } => PrepareReply::Prepared {
            call: call.clone(),
            requested_name: None,
        },
        ExecutionProgress::Settled { outcome } => match outcome {
            ExecutionOutcome::Blocked { value, .. } => stopped_reply(value)
                .map(Into::into)
                .unwrap_or(PrepareReply::Refused {
                    reason: "corrupt_blocked_outcome".to_owned(),
                }),
            _ => PrepareReply::Refused {
                reason: "terminal".to_owned(),
            },
        },
        other => PrepareReply::Refused {
            reason: other.state_str().to_owned(),
        },
    }
}

struct SharedPending<T: Clone> {
    value: Mutex<Option<Result<T, String>>>,
    notify: Notify,
}

impl<T: Clone> SharedPending<T> {
    fn new() -> Self {
        Self {
            value: Mutex::new(None),
            notify: Notify::new(),
        }
    }

    fn set(&self, res: Result<T, String>) {
        *self.value.lock().unwrap_or_else(|p| p.into_inner()) = Some(res);
        self.notify.notify_waiters();
    }

    async fn wait(&self) -> Result<T, String> {
        loop {
            let notified = self.notify.notified();
            if let Some(res) = self.value.lock().unwrap_or_else(|p| p.into_inner()).clone() {
                return res;
            }
            notified.await;
        }
    }
}

struct InflightPrepare {
    admission: String,
    cell: Arc<SharedPending<PrepareReply>>,
}

struct ExecutorInner {
    ledger: Arc<dyn ExecutionLedger>,
    executor: String,
    pipeline: Arc<dyn PreparationPipeline>,
    clock: Arc<dyn Fn() -> i64 + Send + Sync>,
    consent: Option<Arc<dyn ConsentClassifier>>,
    preparations: Mutex<HashMap<String, InflightPrepare>>,
    bodies: Mutex<HashMap<String, PreparedBody>>,
    releases: Mutex<HashMap<String, Arc<SharedPending<ReleaseReply>>>>,
}

impl ExecutorInner {
    async fn step(
        &self,
        key: &ExecutionKey,
        generation: u64,
        event: ExecutionEvent,
    ) -> Result<LedgerReply, String> {
        self.ledger
            .apply(
                ExecutionCommand {
                    key: key.clone(),
                    generation,
                    actor: ExecutionActor {
                        id: self.executor.clone(),
                        approved_workspace: None,
                    },
                    now: (self.clock)(),
                    event,
                },
                None,
            )
            .await
            .map_err(|e| e.to_string())
    }

    async fn stop(
        &self,
        key: &ExecutionKey,
        generation: u64,
        reply: StoppedReply,
    ) -> Result<PrepareReply, String> {
        let recorded = self
            .step(
                key,
                generation,
                ExecutionEvent::Block {
                    value: stopped_value(&reply),
                },
            )
            .await?;
        match &recorded {
            LedgerReply::Applied { .. } => Ok(reply.into()),
            LedgerReply::Replayed { entry } => Ok(from_row(entry)),
            other => Ok(PrepareReply::Refused {
                reason: refusal_from_reply(other),
            }),
        }
    }

    async fn prepare_once(
        &self,
        request: PrepareRequest,
        request_digest: String,
    ) -> Result<PrepareReply, String> {
        let key = key_of(&request);
        let generation = request.generation;
        let call = request.call.clone();
        let admitted = self
            .ledger
            .admit(
                ExecutionAdmission {
                    issuer: key.issuer.clone(),
                    command_id: key.command_id.clone(),
                    executor: self.executor.clone(),
                    workspace: request.workspace.clone(),
                    scope: request.scope.clone(),
                    generation,
                    request_digest: request_digest.clone(),
                    admitted_at: request.admitted_at,
                    lease_until: request.lease_until,
                    reconcilable: request.reconcilable.unwrap_or(false),
                },
                None,
            )
            .await
            .map_err(|e| e.to_string())?;
        match &admitted {
            LedgerReply::Replayed { entry } => return Ok(from_row(entry)),
            LedgerReply::Applied { .. } => {}
            other => {
                return Ok(PrepareReply::Refused {
                    reason: refusal_from_reply(other),
                });
            }
        }

        let preparing = self.step(&key, generation, ExecutionEvent::Prepare).await?;
        if !matches!(preparing, LedgerReply::Applied { .. }) {
            return Ok(PrepareReply::Refused {
                reason: refusal_from_reply(&preparing),
            });
        }

        if call.incomplete == Some(true) {
            return self
                .stop(
                    &key,
                    generation,
                    StoppedReply::Blocked {
                        result: error_result(incomplete_message(
                            &call.name,
                            call.error_message.as_deref(),
                        )),
                        requested_name: None,
                    },
                )
                .await;
        }

        let effect = self.step(&key, generation, ExecutionEvent::Effect).await?;
        if !matches!(effect, LedgerReply::Applied { .. }) {
            return Ok(PrepareReply::Refused {
                reason: refusal_from_reply(&effect),
            });
        }

        let tool = match self.pipeline.resolve(&call, &request.context).await {
            Ok(tool) => tool,
            Err(err) => {
                return self
                    .stop(
                        &key,
                        generation,
                        StoppedReply::Blocked {
                            result: error_result(err),
                            requested_name: None,
                        },
                    )
                    .await;
            }
        };

        let Some(tool) = tool else {
            let text = match self.pipeline.removed_tool_hint(&call.name) {
                None => format!("Tool {} not found", call.name),
                Some(hint) => format!("Tool {} not found. {hint}", call.name),
            };
            return self
                .stop(
                    &key,
                    generation,
                    StoppedReply::Blocked {
                        result: error_result(text),
                        requested_name: None,
                    },
                )
                .await;
        };

        let requested_name = if tool.name() == call.name {
            None
        } else {
            Some(call.name.clone())
        };
        let renamed_call = if requested_name.is_none() {
            call.clone()
        } else {
            let mut next = call.clone();
            next.name = tool.name().to_owned();
            next
        };

        let prepared = match prepare_agent_tool_call(&tool, &renamed_call) {
            Ok(p) => p,
            Err(err) => {
                return self
                    .stop(
                        &key,
                        generation,
                        StoppedReply::Blocked {
                            result: error_result(err),
                            requested_name,
                        },
                    )
                    .await;
            }
        };

        let input = InvocationInput::new();
        let before = match invoke_with_input(input.clone(), || {
            self.pipeline.before_tool_call(
                BeforeToolCallContext {
                    assistant_message: request.assistant_message.clone(),
                    tool_call: prepared.tool_call.clone(),
                    args: prepared.args.clone(),
                    context: Arc::clone(&request.context),
                },
                request.signal.clone(),
            )
        })
        .await
        {
            Ok(before) => before,
            Err(err) => {
                return self
                    .stop(
                        &key,
                        generation,
                        StoppedReply::Blocked {
                            result: error_result(err),
                            requested_name,
                        },
                    )
                    .await;
            }
        };

        if request.signal.as_ref().is_some_and(|s| s.aborted()) {
            return self
                .stop(
                    &key,
                    generation,
                    StoppedReply::BatchAbort {
                        result: error_result("Operation aborted"),
                    },
                )
                .await;
        }

        if let Some(ref b) = before
            && b.block == Some(true)
        {
            let mut result = error_result(
                b.reason
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("Tool execution was blocked"),
            );
            if b.terminate == Some(true) {
                result.terminate = Some(true);
            }
            return self
                .stop(
                    &key,
                    generation,
                    StoppedReply::Blocked {
                        result,
                        requested_name,
                    },
                )
                .await;
        }

        let effective_args = before
            .and_then(|b| b.args)
            .unwrap_or_else(|| prepared.args.clone());
        let mut frozen = freeze_call(tool.name(), &effective_args);
        let raw_frozen = freeze_call(&call.name, &Value::Object(call.arguments.clone()));
        let covered = request.authorized_digest.as_deref() == Some(request_digest.as_str())
            && raw_frozen.digest == frozen.digest;
        let awaiting_consent = self
            .consent
            .as_ref()
            .is_some_and(|c| !covered && c.needs_consent(&frozen.name, &frozen.args));

        let freeze = self
            .step(
                &key,
                generation,
                ExecutionEvent::Freeze {
                    call: frozen.clone(),
                    awaiting_consent,
                },
            )
            .await?;
        if !matches!(freeze, LedgerReply::Applied { .. }) {
            return Ok(PrepareReply::Refused {
                reason: refusal_from_reply(&freeze),
            });
        }

        self.pipeline
            .after_freeze(&effective_args, &mut frozen.args);
        self.bodies
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                key_id(&key),
                PreparedBody {
                    tool,
                    tool_call: prepared.tool_call,
                    args: frozen.args.clone(),
                    assistant_message: request.assistant_message,
                    context: request.context,
                    requested_name: requested_name.clone(),
                    input,
                },
            );

        if awaiting_consent {
            Ok(PrepareReply::PreparedAwaitingConsent {
                call: frozen,
                requested_name,
            })
        } else {
            Ok(PrepareReply::Prepared {
                call: frozen,
                requested_name,
            })
        }
    }

    async fn terminal_row(&self, key: ExecutionKey) -> Result<ReleaseReply, String> {
        let entry = self.ledger.read(key).await.map_err(|e| e.to_string())?;
        let Some(entry) = entry else {
            return Ok(ReleaseReply::Refused {
                reason: "not_found".to_owned(),
            });
        };
        if let ExecutionProgress::Settled {
            outcome: ExecutionOutcome::Blocked { .. },
        } = &entry.progress
        {
            return Ok(ReleaseReply::Refused {
                reason: "blocked".to_owned(),
            });
        }
        Ok(ReleaseReply::Replayed { entry })
    }

    async fn run_released(
        &self,
        request: ReleaseRequest,
        body: PreparedBody,
    ) -> Result<ReleaseReply, String> {
        let key = request.key.clone();
        let generation = request.generation;
        let dispatched = self
            .step(&key, generation, ExecutionEvent::Dispatch)
            .await?;
        match &dispatched {
            LedgerReply::Replayed { entry } => {
                return Ok(ReleaseReply::Replayed {
                    entry: entry.clone(),
                });
            }
            LedgerReply::Applied { entry }
                if matches!(entry.progress, ExecutionProgress::Dispatched { .. }) => {}
            other => {
                return Ok(ReleaseReply::Refused {
                    reason: refusal_from_reply(other),
                });
            }
        }

        let on_update = request
            .on_update_for
            .as_ref()
            .map(|f| f(&body.tool_call))
            .or(request.on_update)
            .unwrap_or_else(|| Arc::new(|_| {}));

        let executed = run_body(&body, request.signal.clone(), on_update).await;
        let finalized =
            finalize_body(&body, executed, self.pipeline.as_ref(), request.signal).await;
        let recorded_outcome = recorded_result(finalized, body.requested_name.as_deref());
        let recorded = self
            .step(
                &key,
                generation,
                ExecutionEvent::Result {
                    value: recorded_outcome.value,
                },
            )
            .await?;

        let id = key_id(&key);
        self.bodies
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        self.preparations
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);
        self.releases
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&id);

        match recorded {
            LedgerReply::Replayed { entry } => Ok(ReleaseReply::Replayed { entry }),
            LedgerReply::Applied { .. } => Ok(ReleaseReply::Settled {
                tool_call: body.tool_call,
                result: recorded_outcome.outcome.result,
                is_error: recorded_outcome.outcome.is_error,
                requested_name: body.requested_name,
            }),
            other => Ok(ReleaseReply::Refused {
                reason: refusal_from_reply(&other),
            }),
        }
    }
}

struct PreparedExecutorHandle {
    inner: Arc<ExecutorInner>,
}

impl PreparedExecutor for PreparedExecutorHandle {
    fn prepare(&self, request: PrepareRequest) -> BoxFuture<'_, Result<PrepareReply, String>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let args_val = Value::Object(request.call.arguments.clone());
            let request_digest = canonical_digest(&json!({
                "name": request.call.name,
                "args": args_val,
                "incomplete": request.call.incomplete,
                "errorMessage": request.call.error_message,
            }));
            let admission = serde_json::to_string(&(
                request.generation,
                &request.workspace,
                &request.scope,
                &request_digest,
                request.admitted_at,
                request.lease_until,
                request.reconcilable.unwrap_or(false),
            ))
            .unwrap_or_default();
            let id = key_id(&key_of(&request));

            let (cell, existing_waiter) = {
                let mut map = inner.preparations.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(pending) = map.get(&id) {
                    if pending.admission == admission {
                        (None, Some(Arc::clone(&pending.cell)))
                    } else {
                        return Ok(PrepareReply::Refused {
                            reason: "identity_conflict".to_owned(),
                        });
                    }
                } else {
                    let cell = Arc::new(SharedPending::new());
                    map.insert(
                        id.clone(),
                        InflightPrepare {
                            admission,
                            cell: Arc::clone(&cell),
                        },
                    );
                    (Some(cell), None)
                }
            };

            if let Some(waiter) = existing_waiter {
                return waiter.wait().await;
            }
            let Some(cell) = cell else {
                return Ok(PrepareReply::Refused {
                    reason: "internal".to_owned(),
                });
            };
            let res = inner.prepare_once(request, request_digest).await;
            let keep = matches!(
                res,
                Ok(PrepareReply::Prepared { .. } | PrepareReply::PreparedAwaitingConsent { .. })
            );
            cell.set(res.clone());
            if !keep {
                let mut map = inner.preparations.lock().unwrap_or_else(|p| p.into_inner());
                if map
                    .get(&id)
                    .is_some_and(|entry| Arc::ptr_eq(&entry.cell, &cell))
                {
                    map.remove(&id);
                }
            }
            res
        })
    }

    fn release(&self, request: ReleaseRequest) -> BoxFuture<'_, Result<ReleaseReply, String>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let released = inner
                .ledger
                .apply(
                    ExecutionCommand {
                        key: request.key.clone(),
                        generation: request.generation,
                        actor: ExecutionActor {
                            id: request.actor_id.clone(),
                            approved_workspace: None,
                        },
                        now: (inner.clock)(),
                        event: ExecutionEvent::Release,
                    },
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
            match &released {
                LedgerReply::Refused {
                    reason: LedgerRefusal::Terminal,
                } => return inner.terminal_row(request.key).await,
                LedgerReply::Refused { .. } | LedgerReply::ResultExpired => {
                    return Ok(ReleaseReply::Refused {
                        reason: refusal_from_reply(&released),
                    });
                }
                _ => {}
            }

            let id = key_id(&request.key);
            let existing = inner
                .releases
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&id)
                .cloned();
            if let Some(waiter) = existing {
                return waiter.wait().await;
            }

            let body = inner
                .bodies
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get(&id)
                .cloned();
            let Some(body) = body else {
                return Ok(ReleaseReply::Refused {
                    reason: "not_prepared".to_owned(),
                });
            };
            if request.signal.as_ref().is_some_and(|s| s.aborted()) {
                return Ok(ReleaseReply::Aborted {
                    result: error_result("Operation aborted"),
                });
            }

            let (existing_waiter, cell) = {
                let mut map = inner.releases.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(existing) = map.get(&id) {
                    (Some(existing.clone()), None)
                } else {
                    let cell = Arc::new(SharedPending::new());
                    map.insert(id.clone(), Arc::clone(&cell));
                    (None, Some(cell))
                }
            };
            if let Some(existing) = existing_waiter {
                return existing.wait().await;
            }
            let cell = cell.expect("release cell created");
            let res = inner.run_released(request, body).await;
            cell.set(res.clone());
            if res.is_err() {
                let mut map = inner.releases.lock().unwrap_or_else(|p| p.into_inner());
                if map.get(&id).is_some_and(|entry| Arc::ptr_eq(entry, &cell)) {
                    map.remove(&id);
                }
            }
            res
        })
    }

    fn consent(&self, request: ConsentRequest) -> BoxFuture<'_, Result<ConsentReply, String>> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move {
            let entry = inner
                .ledger
                .read(request.key.clone())
                .await
                .map_err(|e| e.to_string())?;
            let Some(entry) = entry else {
                return Ok(ConsentReply::Refused {
                    reason: "not_found".to_owned(),
                });
            };
            if entry
                .progress
                .call()
                .is_some_and(|c| c.digest != request.frozen_digest)
            {
                return Ok(ConsentReply::Refused {
                    reason: "frozen_digest_mismatch".to_owned(),
                });
            }
            let reply = inner
                .ledger
                .apply(
                    ExecutionCommand {
                        key: request.key.clone(),
                        generation: request.generation,
                        actor: ExecutionActor {
                            id: request.actor_id,
                            approved_workspace: None,
                        },
                        now: (inner.clock)(),
                        event: ExecutionEvent::Consent,
                    },
                    None,
                )
                .await
                .map_err(|e| e.to_string())?;
            match &reply {
                LedgerReply::Applied { entry: updated }
                    if matches!(updated.progress, ExecutionProgress::Prepared { .. }) =>
                {
                    Ok(ConsentReply::Prepared)
                }
                LedgerReply::Refused {
                    reason: LedgerRefusal::InvalidTransition,
                } if matches!(entry.progress, ExecutionProgress::Prepared { .. }) => {
                    Ok(ConsentReply::Prepared)
                }
                LedgerReply::Applied { entry: updated }
                | LedgerReply::Replayed { entry: updated } => Ok(ConsentReply::Replayed {
                    entry: updated.clone(),
                }),
                LedgerReply::Refused {
                    reason: LedgerRefusal::Terminal,
                } => {
                    let terminal = inner
                        .ledger
                        .read(request.key)
                        .await
                        .map_err(|e| e.to_string())?;
                    Ok(match terminal {
                        None => ConsentReply::Refused {
                            reason: "not_found".to_owned(),
                        },
                        Some(entry) => ConsentReply::Replayed { entry },
                    })
                }
                other => Ok(ConsentReply::Refused {
                    reason: refusal_from_reply(other),
                }),
            }
        })
    }
}

pub fn create_prepared_executor(options: PreparedExecutorOptions) -> Arc<dyn PreparedExecutor> {
    Arc::new(PreparedExecutorHandle {
        inner: Arc::new(ExecutorInner {
            ledger: options.ledger,
            executor: options.executor,
            pipeline: options.pipeline,
            clock: options.clock.unwrap_or_else(|| Arc::new(default_clock)),
            consent: options.consent,
            preparations: Mutex::new(HashMap::new()),
            bodies: Mutex::new(HashMap::new()),
            releases: Mutex::new(HashMap::new()),
        }),
    })
}
