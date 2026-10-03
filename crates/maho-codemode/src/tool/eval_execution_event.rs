use maho_ext_api::{AgentToolResult, ContentBlock};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use super::{call_capture::{EvalToolCallMetric, MAX_AGGREGATED_TOOL_NAMES, MAX_CAPTURED_IDENTIFIER_CODE_POINTS, MAX_ENRICHED_TOOL_CALLS, MAX_RPC_EVENT_BYTES, cap_code_points}, types::EvalLanguage};

pub const EVAL_EXECUTION_EVENT: &str = "senpi.eval.execution";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EvalToolAggregate {
    pub count: usize,
    pub total_duration_ms: f64,
    pub ok_count: usize,
    pub error_count: usize,
    pub pending_count: usize,
}

impl EvalToolAggregate {
    fn add(&mut self, item: &Self) {
        self.count += item.count;
        self.total_duration_ms += item.total_duration_ms;
        self.ok_count += item.ok_count;
        self.error_count += item.error_count;
        self.pending_count += item.pending_count;
    }
}

pub enum EvalExecutionSettleOutcome<'a> {
    Result(&'a AgentToolResult),
    Error(&'a str),
}

pub struct BuildEvalExecutionEventOptions<'a> {
    pub cell_id: &'a str,
    pub language: EvalLanguage,
    pub started_at: f64,
    pub completed_at: f64,
    pub queued_ms: f64,
    pub detached: bool,
    pub metrics: &'a [EvalToolCallMetric],
    pub tool_calls: &'a [Value],
    pub state_error: Option<&'a str>,
    pub outcome: EvalExecutionSettleOutcome<'a>,
}

pub fn build_eval_execution_event_payload(options: BuildEvalExecutionEventOptions<'_>) -> Value {
    let mut aggregates: Vec<(String, EvalToolAggregate)> = Vec::new();
    let mut overflow: Option<EvalToolAggregate> = None;
    let mut pending_count = 0;
    for metric in options.metrics {
        let item = EvalToolAggregate {
            count: 1,
            total_duration_ms: metric.duration_ms.unwrap_or_else(|| (options.completed_at - metric.started_at).max(0.0)),
            ok_count: usize::from(metric.ok == Some(true)),
            error_count: usize::from(metric.ok == Some(false)),
            pending_count: usize::from(metric.ok.is_none()),
        };
        pending_count += item.pending_count;
        if let Some((_, existing)) = aggregates.iter_mut().find(|(name, _)| name == &metric.name) {
            existing.add(&item);
        } else if aggregates.len() < MAX_AGGREGATED_TOOL_NAMES {
            aggregates.push((metric.name.clone(), item));
        } else {
            overflow.get_or_insert_with(EvalToolAggregate::default).add(&item);
        }
    }
    let result = match &options.outcome { EvalExecutionSettleOutcome::Result(result) => Some(*result), EvalExecutionSettleOutcome::Error(_) => None };
    let ok = result.is_some_and(|result| result.details["isError"] != true);
    let names: Vec<_> = aggregates.iter().map(|(name, _)| name.clone()).collect();
    let mut payload = json!({
        "version":1,"detailLevel":"full","cellId":cap_code_points(options.cell_id, MAX_CAPTURED_IDENTIFIER_CODE_POINTS),
        "language":options.language,"ok":ok,"startedAt":options.started_at,"completedAt":options.completed_at,
        "durationMs":(options.completed_at-options.started_at).max(0.0),"queued_ms":options.queued_ms,"detached":options.detached,
        "toolCallCount":options.metrics.len(),"pendingToolCallCount":pending_count,
        "toolCalls":options.tool_calls.iter().take(MAX_ENRICHED_TOOL_CALLS).collect::<Vec<_>>(),
        "distinctToolsCalled":names,"toolAggregates":aggregates.into_iter().map(|(name,item)|(name,json!(item))).collect::<serde_json::Map<_,_>>(),
        "toolAggregatesTruncated":overflow.is_some()
    });
    if let Some(overflow) = overflow { payload["toolAggregateOverflow"] = json!(overflow); }
    if let Some(result) = result && let Some(duration) = result.details.get("durationMs") {
        payload["kernelDurationMs"] = duration.clone();
    }
    if !ok {
        let error = options.state_error.or_else(|| match options.outcome {
            EvalExecutionSettleOutcome::Error(error) => Some(error),
            EvalExecutionSettleOutcome::Result(result) => result.content.iter().find_map(|part| match part {
                ContentBlock::Text(text) if !text.text.is_empty() => Some(text.text.trim_end()),
                _ => None,
            }),
        });
        if let Some(error) = error { payload["error"] = json!(cap_code_points(error,512)); }
    }
    payload
}

pub fn to_eval_execution_rpc_payload(payload: &Value) -> Value {
    let mut candidate = payload.clone();
    candidate.as_object_mut().expect("execution payload is an object").remove("error");
    candidate["detailLevel"] = json!("metadata");
    candidate["rpcTruncated"] = json!(false);
    candidate["toolCalls"] = json!(payload["toolCalls"].as_array().expect("execution payload has calls").iter().map(|call| {
        let mut summary = json!({"name":call["name"],"ok":call["ok"]});
        if let Some(duration) = call.get("durationMs") { summary["durationMs"] = duration.clone(); }
        summary
    }).collect::<Vec<_>>());
    if candidate.to_string().len() <= MAX_RPC_EVENT_BYTES { return candidate; }
    let mut total = EvalToolAggregate::default();
    for aggregate in payload["toolAggregates"].as_object().expect("execution aggregates are an object").values().chain(payload.get("toolAggregateOverflow")) {
        total.add(&serde_json::from_value(aggregate.clone()).expect("execution aggregate has native shape"));
    }
    candidate["rpcTruncated"] = json!(true);
    candidate["toolCalls"] = json!([]);
    candidate["distinctToolsCalled"] = json!([]);
    candidate["toolAggregates"] = json!({});
    candidate["toolAggregatesTruncated"] = json!(true);
    candidate["toolAggregateOverflow"] = json!(total);
    candidate
}
