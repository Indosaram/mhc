//! Port of senpi `packages/agent/src/execution/plane-session.ts`.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use maho_ai::types::{BoxFuture, ToolResultMessage};
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::types::{
    AfterToolCall, AgentEvent, AgentToolCall, AgentToolResult, BatchStoreFactory, BeforeToolCall,
    ResolveUnknownToolCall,
};

use super::plane::{NestedRequestFn, PlaneHandler, PlaneRefusedError};
use super::prepared::executor_pipeline::PreparationSource;
use super::prepared::pipeline_prepared::create_prepared_tool_host;
use super::tool_host::{
    AfterToolFreeze, BatchStoreMissingError, ExecutionScope, PreflightCarryMissingError, ToolHost,
    ToolHostCall, ToolHostOptions, ToolHostResult,
};

pub const SESSION_PLANE: &str = "session";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolRunLocation {
    Brain,
    Hand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolLocality {
    pub name: String,
    pub runs: Option<ToolRunLocation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionPlaneRequest {
    pub tool: String,
    pub arguments: serde_json::Map<String, Value>,
}

pub fn decode_session_plane_request(
    body: &Value,
) -> Result<SessionPlaneRequest, PlaneRefusedError> {
    let Some(obj) = body.as_object() else {
        return Err(PlaneRefusedError::new("malformed_request: tool"));
    };
    let Some(tool) = obj
        .get("tool")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return Err(PlaneRefusedError::new("malformed_request: tool"));
    };
    let Some(arguments) = obj.get("arguments").and_then(Value::as_object) else {
        return Err(PlaneRefusedError::new("malformed_request: arguments"));
    };
    Ok(SessionPlaneRequest {
        tool: tool.to_owned(),
        arguments: arguments.clone(),
    })
}

pub fn loop_side_tool_names<'a, I>(tools: I) -> HashSet<String>
where
    I: IntoIterator<Item = &'a ToolLocality>,
{
    tools
        .into_iter()
        .filter(|tool| tool.runs == Some(ToolRunLocation::Brain))
        .map(|tool| tool.name.clone())
        .collect()
}

pub type SessionPlaneRunFn = Arc<
    dyn Fn(SessionPlaneRequest, AbortSignal) -> BoxFuture<'static, Result<AgentToolResult, String>>
        + Send
        + Sync,
>;

pub struct ServeSessionPlaneOptions {
    pub loop_side: HashSet<String>,
    pub run: SessionPlaneRunFn,
}

pub fn serve_session_plane(options: ServeSessionPlaneOptions) -> PlaneHandler {
    let loop_side = Arc::new(options.loop_side);
    let run = options.run;
    Arc::new(move |body, signal| {
        let loop_side = Arc::clone(&loop_side);
        let run = Arc::clone(&run);
        Box::pin(async move {
            let req = decode_session_plane_request(&body).map_err(|e| e.to_string())?;
            if !loop_side.contains(&req.tool) {
                return Err(format!("not_loop_side: {}", req.tool));
            }
            let result = run(req, signal).await?;
            serde_json::to_value(result).map_err(|e| e.to_string())
        })
    })
}

#[derive(Clone)]
pub struct SessionPlaneClient {
    nested: NestedRequestFn,
}

impl SessionPlaneClient {
    pub async fn call(
        &self,
        request: SessionPlaneRequest,
        signal: Option<AbortSignal>,
    ) -> Result<AgentToolResult, PlaneRefusedError> {
        let body =
            serde_json::to_value(request).map_err(|e| PlaneRefusedError::new(e.to_string()))?;
        let reply = (self.nested)(SESSION_PLANE, body, signal).await?;
        let obj = reply
            .as_object()
            .ok_or_else(|| PlaneRefusedError::new("malformed_reply"))?;
        if !obj.get("content").is_some_and(Value::is_array) {
            return Err(PlaneRefusedError::new("malformed_reply"));
        }
        serde_json::from_value(reply).map_err(|_| PlaneRefusedError::new("malformed_reply"))
    }
}

pub fn create_session_plane_client(nested: NestedRequestFn) -> SessionPlaneClient {
    SessionPlaneClient { nested }
}

#[derive(Clone, Default)]
pub struct LoopToolSource {
    pub resolve_unknown_tool_call: Option<ResolveUnknownToolCall>,
    pub before_tool_call: Option<BeforeToolCall>,
    pub after_tool_call: Option<AfterToolCall>,
    pub removed_tool_hints: Option<HashMap<String, String>>,
    pub after_tool_freeze: Option<AfterToolFreeze>,
    pub batch_store_factory: Option<BatchStoreFactory>,
}

pub type LoopToolSourceReader = Arc<dyn Fn() -> LoopToolSource + Send + Sync>;

struct LoopToolHost {
    read_source: LoopToolSourceReader,
}

impl ToolHost for LoopToolHost {
    fn execute(
        &self,
        call: ToolHostCall,
        options: ToolHostOptions,
    ) -> BoxFuture<'_, Result<ToolHostResult, String>> {
        let source = (self.read_source)();
        Box::pin(async move {
            let batch_store_factory = source
                .batch_store_factory
                .ok_or_else(|| BatchStoreMissingError.to_string())?;
            let after_tool_freeze = source
                .after_tool_freeze
                .ok_or_else(|| PreflightCarryMissingError.to_string())?;
            let prep_source = PreparationSource {
                resolve_unknown_tool_call: source.resolve_unknown_tool_call,
                before_tool_call: source.before_tool_call,
                after_tool_call: source.after_tool_call,
                removed_tool_hints: source.removed_tool_hints,
                after_tool_freeze,
            };
            create_prepared_tool_host(prep_source, batch_store_factory)
                .execute(call, options)
                .await
        })
    }
}

pub fn create_loop_tool_host(read_source: LoopToolSourceReader) -> Arc<dyn ToolHost> {
    Arc::new(LoopToolHost { read_source })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    Loop,
    Hand,
}

struct Segment {
    side: Side,
    calls: Vec<AgentToolCall>,
}

fn stages_of<F, G>(
    calls: &[AgentToolCall],
    side_of: &F,
    is_sequential: &G,
    parallel: bool,
) -> (Vec<Vec<usize>>, Vec<Segment>, Vec<usize>)
where
    F: Fn(&AgentToolCall) -> Side,
    G: Fn(&AgentToolCall) -> bool,
{
    let mut stages: Vec<Vec<usize>> = Vec::new();
    let mut segments: Vec<Segment> = Vec::new();
    let mut owner: Vec<usize> = Vec::with_capacity(calls.len());
    let mut open = false;

    for call in calls {
        let side = side_of(call);
        let barrier = parallel && is_sequential(call);
        let joinable = open
            && !barrier
            && stages.last().is_some_and(|stage| {
                parallel || stage.first().is_some_and(|&idx| segments[idx].side == side)
            });
        if !joinable {
            stages.push(Vec::new());
        }
        let stage = stages.last_mut().expect("pushed above");
        let existing_idx = stage
            .iter()
            .copied()
            .find(|&idx| segments[idx].side == side);
        let seg_idx = match existing_idx {
            Some(idx) => idx,
            None => {
                let idx = segments.len();
                segments.push(Segment {
                    side,
                    calls: Vec::new(),
                });
                stage.push(idx);
                idx
            }
        };
        segments[seg_idx].calls.push(call.clone());
        owner.push(seg_idx);
        open = !barrier;
    }

    (stages, segments, owner)
}

struct HeldResult {
    tool_call_id: String,
    events: Vec<AgentEvent>,
}

fn holding_results(
    emit: Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync>,
    held: Arc<Mutex<Vec<HeldResult>>>,
) -> Arc<dyn Fn(AgentEvent) -> BoxFuture<'static, ()> + Send + Sync> {
    Arc::new(move |event: AgentEvent| {
        let emit = Arc::clone(&emit);
        let held = Arc::clone(&held);
        Box::pin(async move {
            let tool_call_id_opt = match &event {
                AgentEvent::MessageStart { message, .. } | AgentEvent::MessageEnd { message } => {
                    message.as_tool_result().map(|tr| tr.tool_call_id.clone())
                }
                _ => None,
            };
            let Some(tool_call_id) = tool_call_id_opt else {
                emit(event).await;
                return;
            };
            let mut guard = held.lock().unwrap_or_else(|p| p.into_inner());
            if matches!(event, AgentEvent::MessageEnd { .. })
                && let Some(open) = guard.iter_mut().rfind(|e| e.tool_call_id == tool_call_id)
                && open
                    .events
                    .last()
                    .is_some_and(|last| matches!(last, AgentEvent::MessageStart { .. }))
            {
                open.events.push(event);
                return;
            }
            guard.push(HeldResult {
                tool_call_id,
                events: vec![event],
            });
        })
    })
}

pub struct SplitToolHostOptions {
    pub loop_side: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub is_sequential: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    pub loop_host: Arc<dyn ToolHost>,
    pub hand_host: Arc<dyn ToolHost>,
}

struct SplitToolHost {
    loop_side: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    is_sequential: Arc<dyn Fn(&str) -> bool + Send + Sync>,
    loop_host: Arc<dyn ToolHost>,
    hand_host: Arc<dyn ToolHost>,
}

impl ToolHost for SplitToolHost {
    fn execute(
        &self,
        call: ToolHostCall,
        host_options: ToolHostOptions,
    ) -> BoxFuture<'_, Result<ToolHostResult, String>> {
        let loop_side = Arc::clone(&self.loop_side);
        let is_sequential = Arc::clone(&self.is_sequential);
        let loop_host = Arc::clone(&self.loop_host);
        let hand_host = Arc::clone(&self.hand_host);
        Box::pin(async move {
            let parallel = call.mode == crate::types::ToolExecutionMode::Parallel;
            let (stages, segments, owner) = stages_of(
                &call.calls,
                &|c: &AgentToolCall| {
                    if loop_side(&c.name) {
                        Side::Loop
                    } else {
                        Side::Hand
                    }
                },
                &|c: &AgentToolCall| is_sequential(&c.name),
                parallel,
            );
            let first_side = segments.first().map(|s| s.side);
            if segments.iter().all(|s| Some(s.side) == first_side) {
                let host = match first_side.unwrap_or(Side::Hand) {
                    Side::Loop => loop_host,
                    Side::Hand => hand_host,
                };
                return host.execute(call, host_options).await;
            }

            let held_per_segment: Vec<Arc<Mutex<Vec<HeldResult>>>> = (0..segments.len())
                .map(|_| Arc::new(Mutex::new(Vec::new())))
                .collect();
            let results: Arc<Mutex<Vec<Option<ToolHostResult>>>> =
                Arc::new(Mutex::new(vec![None; segments.len()]));

            for stage in stages {
                let mut futs = Vec::with_capacity(stage.len());
                for seg_idx in stage {
                    let seg = &segments[seg_idx];
                    let host = match seg.side {
                        Side::Loop => Arc::clone(&loop_host),
                        Side::Hand => Arc::clone(&hand_host),
                    };
                    let seg_call = ToolHostCall {
                        scope: call.scope.clone(),
                        calls: seg.calls.clone(),
                        mode: call.mode,
                    };
                    let emit = if parallel {
                        holding_results(
                            Arc::clone(&host_options.emit),
                            Arc::clone(&held_per_segment[seg_idx]),
                        )
                    } else {
                        Arc::clone(&host_options.emit)
                    };
                    let seg_opts = ToolHostOptions {
                        signal: host_options.signal.clone(),
                        emit,
                        record_witness: host_options.record_witness.clone(),
                        local: host_options.local.clone(),
                    };
                    let results_ref = Arc::clone(&results);
                    futs.push(async move {
                        let res = host.execute(seg_call, seg_opts).await?;
                        results_ref.lock().unwrap_or_else(|p| p.into_inner())[seg_idx] = Some(res);
                        Ok::<(), String>(())
                    });
                }
                for res in futures::future::join_all(futs).await {
                    res?;
                }
                if host_options
                    .signal
                    .as_ref()
                    .is_some_and(AbortSignal::aborted)
                {
                    break;
                }
            }

            let mut message_queues: Vec<
                HashMap<String, std::collections::VecDeque<ToolResultMessage>>,
            > = (0..segments.len()).map(|_| HashMap::new()).collect();
            let mut ran_flags = Vec::new();
            {
                let mut results_guard = results.lock().unwrap_or_else(|p| p.into_inner());
                for (seg_idx, res_opt) in results_guard.iter_mut().enumerate() {
                    if let Some(res) = res_opt {
                        if !res.messages.is_empty() {
                            ran_flags.push(res.terminate);
                        }
                        for msg in std::mem::take(&mut res.messages) {
                            message_queues[seg_idx]
                                .entry(msg.tool_call_id.clone())
                                .or_default()
                                .push_back(msg);
                        }
                    }
                }
            }

            let mut event_queues: Vec<HashMap<String, std::collections::VecDeque<HeldResult>>> =
                (0..segments.len()).map(|_| HashMap::new()).collect();
            for (seg_idx, held_arc) in held_per_segment.iter().enumerate() {
                let mut held = held_arc.lock().unwrap_or_else(|p| p.into_inner());
                for item in held.drain(..) {
                    event_queues[seg_idx]
                        .entry(item.tool_call_id.clone())
                        .or_default()
                        .push_back(item);
                }
            }

            let mut ordered = Vec::new();
            for (index, raw) in call.calls.iter().enumerate() {
                let seg_idx = owner[index];
                if let Some(queue) = event_queues[seg_idx].get_mut(&raw.id)
                    && let Some(held) = queue.pop_front()
                {
                    for ev in held.events {
                        (host_options.emit)(ev).await;
                    }
                }
                if let Some(queue) = message_queues[seg_idx].get_mut(&raw.id)
                    && let Some(msg) = queue.pop_front()
                {
                    ordered.push(msg);
                }
            }

            let terminate = !ran_flags.is_empty() && ran_flags.into_iter().all(|t| t);
            Ok(ToolHostResult {
                messages: ordered,
                terminate,
            })
        })
    }

    fn device_files(
        &self,
        scope: &ExecutionScope,
    ) -> Option<Arc<dyn super::device_files::DeviceFiles>> {
        self.hand_host.device_files(scope)
    }
}

pub fn create_split_tool_host(options: SplitToolHostOptions) -> Arc<dyn ToolHost> {
    Arc::new(SplitToolHost {
        loop_side: options.loop_side,
        is_sequential: options.is_sequential,
        loop_host: options.loop_host,
        hand_host: options.hand_host,
    })
}
