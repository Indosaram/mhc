//! Port of senpi `packages/agent/src/execution/prepared/pipeline-prepared-batch.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use maho_ai::types::{AssistantMessage, BoxFuture};
use maho_ai::utils::abort::AbortSignal;
use serde_json::Value;
use uuid::Uuid;

use crate::assistant_terminal_state::now_ms;
use crate::durable::store_admission::create_admission_line;
use crate::execution::coordinator::{BatchCoordinatorOptions, create_batch_coordinator};
use crate::execution::coordinator_types::{
    BatchCallPlan, BatchExecutor, BatchPlan, ExecutionRef, PrepareReply as CoordinatorPrepareReply,
    PrepareRequest as CoordinatorPrepareRequest, SettleReply,
};
use crate::execution::ledger::{StoreExecutionLedgerOptions, create_execution_ledger};
use crate::execution::ledger_types::{ExecutionKey, ExecutionOutcome, InterruptedReason};
use crate::execution::tool_host::ExecutionScope;
use crate::execution::tool_outcome_events::{
    create_error_tool_result, emit_tool_execution_end, emit_tool_execution_update,
};
use crate::tool_name_alias::{resolve_tool_name_alias, with_tool_name_correction};
use crate::types::{
    AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent, AgentTool, AgentToolCall,
    AgentToolCallOutcome, BatchStoreFactory, BeforeToolCallContext, BeforeToolCallResult,
    ToolExecutionMode,
};

use super::executor::create_prepared_executor;
use super::executor_freeze::to_store_value;
use super::executor_pipeline::{PreparationSource, agent_preparation_pipeline};
use super::executor_types::{
    PreparationPipeline, PrepareReply as PreparedPrepareReply,
    PrepareRequest as PreparedPrepareRequest, ReleaseReply, ReleaseRequest,
};

const ISSUER: &str = "loop";
const EXECUTOR: &str = "local";
const LEASE_MS: i64 = 24 * 60 * 60 * 1000;
const NO_WORKSPACE: &str = "in-process";

pub type EmitFn = Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>;

pub struct PreparedBatchRequest {
    pub source: PreparationSource,
    pub calls: Vec<AgentToolCall>,
    pub scope: ExecutionScope,
    pub context: Arc<AgentContext>,
    pub assistant_message: AssistantMessage,
    pub emit: EmitFn,
    pub signal: Option<AbortSignal>,
    pub open_store: BatchStoreFactory,
}

fn is_sequential(context: &AgentContext, name: &str) -> bool {
    let Some(tools) = context.tools.as_ref() else {
        return false;
    };
    let available: Vec<String> = tools.iter().map(|t| t.name().to_owned()).collect();
    let named = resolve_tool_name_alias(name, available);
    let tool = tools.iter().find(|c| c.name() == name).or_else(|| {
        named
            .as_deref()
            .and_then(|n| tools.iter().find(|c| c.name() == n))
    });
    tool.is_some_and(|t| t.execution_mode == Some(ToolExecutionMode::Sequential))
}

struct CachedResolvePipeline {
    base: Arc<dyn PreparationPipeline>,
    current_resolved: Arc<Mutex<Option<Option<AgentTool>>>>,
}

impl PreparationPipeline for CachedResolvePipeline {
    fn resolve(
        &self,
        _call: &AgentToolCall,
        _context: &AgentContext,
    ) -> BoxFuture<'_, Result<Option<AgentTool>, String>> {
        let resolved = self
            .current_resolved
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
            .flatten();
        Box::pin(async move { Ok(resolved) })
    }

    fn before_tool_call(
        &self,
        context: BeforeToolCallContext,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Option<BeforeToolCallResult>, String>> {
        self.base.before_tool_call(context, signal)
    }

    fn after_tool_call(
        &self,
        context: AfterToolCallContext,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Option<AfterToolCallResult>, String>> {
        self.base.after_tool_call(context, signal)
    }

    fn removed_tool_hint(&self, name: &str) -> Option<String> {
        self.base.removed_tool_hint(name)
    }

    fn after_freeze(&self, prepared: &Value, frozen: &mut Value) {
        self.base.after_freeze(prepared, frozen);
    }
}

struct PreparedBatchEndpoint {
    prepared: Arc<dyn super::executor_types::PreparedExecutor>,
    base: Arc<dyn PreparationPipeline>,
    current_resolved: Arc<Mutex<Option<Option<AgentTool>>>>,
    by_execution: HashMap<String, AgentToolCall>,
    context: Arc<AgentContext>,
    assistant_message: AssistantMessage,
    emit: EmitFn,
    signal: Option<AbortSignal>,
    renamed: Mutex<HashMap<String, AgentToolCall>>,
    started: Mutex<HashSet<String>>,
    pending: Mutex<HashMap<String, AgentToolCallOutcome>>,
    finalized: Mutex<HashMap<String, AgentToolCallOutcome>>,
}

impl PreparedBatchEndpoint {
    fn named(&self, id: &str, call: &AgentToolCall) -> AgentToolCall {
        self.renamed
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(id)
            .cloned()
            .unwrap_or_else(|| call.clone())
    }

    fn call_of(&self, id: &str) -> Result<AgentToolCall, String> {
        self.by_execution
            .get(id)
            .cloned()
            .ok_or_else(|| format!("prepared pipeline: unknown execution {id}"))
    }

    async fn finish(&self, id: &str, outcome: AgentToolCallOutcome) {
        self.finalized
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(id.to_owned(), outcome.clone());
        emit_tool_execution_end(&outcome, &self.emit).await;
    }

    fn aborted_outcome(call: AgentToolCall) -> AgentToolCallOutcome {
        AgentToolCallOutcome {
            tool_call: call,
            result: create_error_tool_result("Operation aborted"),
            is_error: true,
        }
    }
}

impl BatchExecutor for PreparedBatchEndpoint {
    fn prepare(
        &self,
        request: CoordinatorPrepareRequest,
    ) -> BoxFuture<'_, Result<CoordinatorPrepareReply, String>> {
        Box::pin(async move {
            let id = request.execution_id.clone();
            let call = self.call_of(&id)?;
            let aborted_now = self.signal.as_ref().is_some_and(AbortSignal::aborted);
            let started_count = self.started.lock().unwrap_or_else(|p| p.into_inner()).len();
            if aborted_now && started_count > 0 {
                return Ok(CoordinatorPrepareReply::BatchAbort);
            }
            let tool = if call.incomplete == Some(true) {
                None
            } else {
                self.base.resolve(&call, &self.context).await?
            };
            *self
                .current_resolved
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = Some(tool.clone());
            if let Some(ref resolved_tool) = tool
                && resolved_tool.name() != call.name
            {
                let mut renamed_call = call.clone();
                renamed_call.name = resolved_tool.name().to_owned();
                self.renamed
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id.clone(), renamed_call);
            }
            (self.emit)(AgentEvent::ToolExecutionStart {
                parent_tool_call_id: None,
                tool_call_id: call.id.clone(),
                tool_name: tool
                    .as_ref()
                    .map_or_else(|| call.name.clone(), |t| t.name().to_owned()),
                args: Value::Object(call.arguments.clone()),
            })
            .await;
            self.started
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(id.clone());

            let reply = self
                .prepared
                .prepare(PreparedPrepareRequest {
                    issuer: ISSUER.to_owned(),
                    command_id: Some(id.clone()),
                    generation: request.generation,
                    workspace: request.workspace,
                    scope: request.scope,
                    call: call.clone(),
                    assistant_message: self.assistant_message.clone(),
                    context: Arc::clone(&self.context),
                    admitted_at: request.admitted_at,
                    lease_until: request.lease_until,
                    reconcilable: None,
                    signal: self.signal.clone(),
                    authorized_digest: None,
                })
                .await?;

            if self.signal.as_ref().is_some_and(AbortSignal::aborted) {
                let outcome = Self::aborted_outcome(self.named(&id, &call));
                self.pending
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(id, outcome);
                return Ok(CoordinatorPrepareReply::BatchAbort);
            }

            match reply {
                PreparedPrepareReply::Prepared { call, .. } => {
                    Ok(CoordinatorPrepareReply::Prepared {
                        frozen_digest: call.digest,
                    })
                }
                PreparedPrepareReply::PreparedAwaitingConsent { call, .. } => {
                    Ok(CoordinatorPrepareReply::PreparedAwaitingConsent {
                        frozen_digest: call.digest,
                    })
                }
                PreparedPrepareReply::Refused { reason } => Err(format!(
                    "prepared pipeline refused call {} (execution {id}): {reason}",
                    call.id
                )),
                PreparedPrepareReply::Blocked {
                    result,
                    requested_name,
                } => {
                    let result = match (requested_name, tool) {
                        (Some(req_name), Some(resolved_tool)) => {
                            with_tool_name_correction(result, &req_name, resolved_tool.name())
                        }
                        _ => result,
                    };
                    let val = to_store_value(&serde_json::json!({ "content": result.content }))
                        .unwrap_or(Value::Null);
                    self.pending
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(
                            id.clone(),
                            AgentToolCallOutcome {
                                tool_call: self.named(&id, &call),
                                result,
                                is_error: true,
                            },
                        );
                    Ok(CoordinatorPrepareReply::Blocked { result: val })
                }
                PreparedPrepareReply::BatchAbort { result } => {
                    let outcome = AgentToolCallOutcome {
                        tool_call: self.named(&id, &call),
                        result,
                        is_error: true,
                    };
                    self.pending
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .insert(id, outcome);
                    Ok(CoordinatorPrepareReply::BatchAbort)
                }
            }
        })
    }

    fn release(&self, exec_ref: ExecutionRef) -> BoxFuture<'_, Result<SettleReply, String>> {
        Box::pin(async move {
            let id = exec_ref.execution_id.clone();
            let call = self.call_of(&id)?;
            let emit_for_update = Arc::clone(&self.emit);
            let reply = self
                .prepared
                .release(ReleaseRequest {
                    key: ExecutionKey {
                        issuer: ISSUER.to_owned(),
                        command_id: id.clone(),
                    },
                    generation: exec_ref.generation,
                    actor_id: ISSUER.to_owned(),
                    signal: self.signal.clone(),
                    on_update: None,
                    on_update_for: Some(Arc::new(move |tc| {
                        emit_tool_execution_update(tc.clone(), Arc::clone(&emit_for_update))
                    })),
                })
                .await?;

            match reply {
                ReleaseReply::Settled {
                    tool_call,
                    result,
                    is_error,
                    requested_name,
                } => {
                    let result = match requested_name {
                        Some(req) => with_tool_name_correction(result, &req, &tool_call.name),
                        None => result,
                    };
                    let content_val = to_store_value(&result.content).unwrap_or(Value::Null);
                    self.finish(
                        &id,
                        AgentToolCallOutcome {
                            tool_call,
                            result,
                            is_error,
                        },
                    )
                    .await;
                    Ok(SettleReply::Settled {
                        outcome: ExecutionOutcome::Result {
                            value: content_val,
                            hooks_ran: true,
                            uncertain: false,
                        },
                    })
                }
                ReleaseReply::Aborted { result } => {
                    let outcome = if self.signal.as_ref().is_some_and(AbortSignal::aborted) {
                        Self::aborted_outcome(self.named(&id, &call))
                    } else {
                        AgentToolCallOutcome {
                            tool_call: self.named(&id, &call),
                            result,
                            is_error: true,
                        }
                    };
                    self.finish(&id, outcome).await;
                    Ok(SettleReply::Settled {
                        outcome: ExecutionOutcome::from_reason(
                            InterruptedReason::Cancelled,
                            true,
                            false,
                        ),
                    })
                }
                ReleaseReply::Replayed { .. } => Err(format!(
                    "prepared pipeline: release replayed for call {} (execution {id})",
                    call.id
                )),
                ReleaseReply::Refused { .. } => Err(format!(
                    "prepared pipeline: release refused for call {} (execution {id})",
                    call.id
                )),
            }
        })
    }

    fn abort(&self, exec_ref: ExecutionRef) -> BoxFuture<'_, Result<SettleReply, String>> {
        Box::pin(async move {
            let id = exec_ref.execution_id;
            let call = self.call_of(&id)?;
            let was_started = self
                .started
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .contains(&id);
            if was_started {
                let outcome = if self.signal.as_ref().is_some_and(AbortSignal::aborted) {
                    Self::aborted_outcome(self.named(&id, &call))
                } else {
                    self.pending
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .remove(&id)
                        .unwrap_or_else(|| Self::aborted_outcome(self.named(&id, &call)))
                };
                self.finish(&id, outcome).await;
            }
            Ok(SettleReply::Settled {
                outcome: ExecutionOutcome::from_reason(InterruptedReason::Cancelled, false, false),
            })
        })
    }
}

pub async fn run_prepared_batch(
    request: PreparedBatchRequest,
) -> Result<Vec<AgentToolCallOutcome>, String> {
    let PreparedBatchRequest {
        source,
        calls,
        scope,
        context,
        assistant_message,
        emit,
        signal,
        open_store,
    } = request;
    let store = open_store();
    store.claim(0, 1).await.map_err(|e| e.to_string())?;
    let admission = create_admission_line();
    let base = agent_preparation_pipeline(source);
    let current_resolved = Arc::new(Mutex::new(None));
    let ledger = create_execution_ledger(StoreExecutionLedgerOptions {
        store: Arc::clone(&store),
        epoch: 1,
        admission: admission.clone(),
    });
    let prepared = create_prepared_executor(super::executor_types::PreparedExecutorOptions {
        ledger,
        executor: EXECUTOR.to_owned(),
        pipeline: Arc::new(CachedResolvePipeline {
            base: Arc::clone(&base),
            current_resolved: Arc::clone(&current_resolved),
        }),
        clock: None,
        consent: None,
    });

    let mut by_execution = HashMap::new();
    for (index, call) in calls.iter().enumerate() {
        by_execution.insert(index.to_string(), call.clone());
    }

    let endpoint = Arc::new(PreparedBatchEndpoint {
        prepared,
        base,
        current_resolved,
        by_execution,
        context: Arc::clone(&context),
        assistant_message,
        emit,
        signal,
        renamed: Mutex::new(HashMap::new()),
        started: Mutex::new(HashSet::new()),
        pending: Mutex::new(HashMap::new()),
        finalized: Mutex::new(HashMap::new()),
    });

    let now = now_ms();
    let workspace = if scope.root.is_empty() {
        NO_WORKSPACE.to_owned()
    } else {
        scope.root.clone()
    };
    let plan = BatchPlan {
        batch_id: Uuid::new_v4().to_string(),
        scope: scope.id,
        workspace,
        generation: 1,
        admitted_at: now,
        lease_until: now + LEASE_MS,
        calls: calls
            .iter()
            .enumerate()
            .map(|(index, call)| BatchCallPlan {
                execution_id: index.to_string(),
                executor: EXECUTOR.to_owned(),
                name: call.name.clone(),
                args: Value::Object(call.arguments.clone()),
                sequential: is_sequential(&context, &call.name),
            })
            .collect(),
    };

    let endpoint_clone: Arc<dyn BatchExecutor> = Arc::clone(&endpoint) as Arc<dyn BatchExecutor>;
    let coordinator = create_batch_coordinator(BatchCoordinatorOptions {
        issuer: ISSUER.to_owned(),
        store,
        epoch: 1,
        admission,
        executor: Arc::new(move |_| Arc::clone(&endpoint_clone)),
        consent: None,
    });
    coordinator.run(plan).await?;

    let started = endpoint.started.lock().unwrap_or_else(|p| p.into_inner());
    let mut finalized = endpoint.finalized.lock().unwrap_or_else(|p| p.into_inner());
    let mut outcomes = Vec::new();
    for index in 0..calls.len() {
        let id = index.to_string();
        if started.contains(&id)
            && let Some(outcome) = finalized.remove(&id)
        {
            outcomes.push(outcome);
        }
    }
    Ok(outcomes)
}
