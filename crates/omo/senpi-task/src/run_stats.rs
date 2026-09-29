//! Run facts accumulated from a managed child's event stream (`run-stats.ts`).

use std::collections::HashSet;

use serde_json::{Map, Value};

use crate::shared::ManagedChildEvent;
use crate::state::TaskRunStats;

/// Accumulates run facts. A generation window opens at the last boundary (spawn, assistant
/// `message_start`, `tool_execution_end`, previous `message_end`) and closes on an assistant
/// `message_end`, so `tokens_per_second` measures streaming speed and excludes tool time.
pub struct RunStatsTracker<C: Fn() -> u64> {
    started_at: u64,
    now: C,
    turns: u64,
    tool_calls: u64,
    output_tokens: f64,
    total_tokens: f64,
    generation_ms: u64,
    collapsed_windows: u64,
    window_start: u64,
    cost_usd: f64,
    saw_cost: bool,
    cache_read_tokens: f64,
    cacheable_tokens: f64,
    latest_cache_hit_rate: Option<f64>,
    eval_run_call_ids: HashSet<String>,
    anonymous_eval_runs: u64,
}

pub fn create_run_stats_tracker<C: Fn() -> u64>(started_at: u64, now: C) -> RunStatsTracker<C> {
    RunStatsTracker {
        started_at,
        now,
        turns: 0,
        tool_calls: 0,
        output_tokens: 0.0,
        total_tokens: 0.0,
        generation_ms: 0,
        collapsed_windows: 0,
        window_start: started_at,
        cost_usd: 0.0,
        saw_cost: false,
        cache_read_tokens: 0.0,
        cacheable_tokens: 0.0,
        latest_cache_hit_rate: None,
        eval_run_call_ids: HashSet::new(),
        anonymous_eval_runs: 0,
    }
}

impl<C: Fn() -> u64> RunStatsTracker<C> {
    /// Folds one event in; `true` when the event changed the reported stats.
    pub fn accept(&mut self, event: &ManagedChildEvent) -> bool {
        match event.event_type.as_str() {
            "tool_execution_start" => {
                self.tool_calls += 1;
                let eval_input = event.args.as_ref().or(event.input.as_ref());
                let is_eval_run = event.tool_name.as_deref() == Some("eval")
                    && eval_input.and_then(Value::as_object).is_none_or(|input| {
                        let action = input.get("action").and_then(Value::as_str);
                        action != Some("peek") && action != Some("stop")
                    });
                if is_eval_run {
                    match &event.tool_call_id {
                        None => self.anonymous_eval_runs += 1,
                        Some(id) => {
                            self.eval_run_call_ids.insert(id.clone());
                        }
                    }
                }
                true
            }
            "tool_execution_end" => {
                let mut count_nested_eval_tools = false;
                if event.tool_name.as_deref() == Some("eval") {
                    if let Some(id) = &event.tool_call_id {
                        count_nested_eval_tools = self.eval_run_call_ids.remove(id);
                    } else if self.anonymous_eval_runs > 0 {
                        self.anonymous_eval_runs -= 1;
                        count_nested_eval_tools = true;
                    }
                }
                let nested_eval_tools = if count_nested_eval_tools {
                    event
                        .result
                        .as_ref()
                        .and_then(Value::as_object)
                        .and_then(|result| result.get("details"))
                        .and_then(Value::as_object)
                        .and_then(|details| details.get("toolCalls"))
                        .and_then(Value::as_array)
                        .map_or(0, |calls| {
                            calls
                                .iter()
                                .filter(|call| is_nested_tool_call(call))
                                .count()
                        })
                } else {
                    0
                };
                self.tool_calls += nested_eval_tools as u64;
                self.window_start = (self.now)();
                nested_eval_tools > 0
            }
            "message_start" => {
                if assistant_message(event.message.as_ref()).is_some() {
                    self.window_start = (self.now)();
                }
                false
            }
            "message_end" => {
                let Some(message) = assistant_message(event.message.as_ref()) else {
                    return false;
                };
                self.close_window(message);
                true
            }
            _ => false,
        }
    }

    fn close_window(&mut self, message: &Map<String, Value>) {
        // Windows use the arrival clock: `AssistantMessage.timestamp` marks stream start, not
        // completion, so it cannot close a generation window.
        let timestamp = (self.now)();
        self.turns += 1;
        let window = timestamp.saturating_sub(self.window_start);
        self.generation_ms += window;
        self.window_start = timestamp;
        let usage = read_usage(message);
        if window == 0 && usage.output.unwrap_or(0.0) > 0.0 {
            self.collapsed_windows += 1;
        }
        self.output_tokens += usage.output.unwrap_or(0.0);
        self.total_tokens += usage.total.unwrap_or(0.0);
        if let Some(cost) = usage.cost {
            self.cost_usd += cost;
            self.saw_cost = true;
        }
        let request_cache_read = usage.cache_read.unwrap_or(0.0);
        let request_cacheable =
            usage.input.unwrap_or(0.0) + request_cache_read + usage.cache_write.unwrap_or(0.0);
        if let Some(rate) = bounded_cache_hit_rate(request_cache_read, request_cacheable) {
            self.latest_cache_hit_rate = Some(rate);
        }
        self.cache_read_tokens += request_cache_read;
        self.cacheable_tokens += request_cacheable;
    }

    /// `tokens_per_second` is emitted only when every token-bearing window had a non-zero
    /// duration; a collapsed window loses its timing, so any figure would be unverifiable. The
    /// runtime fallback applies only when no window was measured and none collapsed.
    pub fn snapshot(&self, now_ms: u64) -> TaskRunStats {
        let runtime_ms = now_ms.saturating_sub(self.started_at);
        let throughput_window_ms = if self.collapsed_windows > 0 {
            None
        } else if self.generation_ms > 0 {
            Some(self.generation_ms)
        } else {
            Some(runtime_ms)
        };
        let positive = |value: f64| (value > 0.0).then_some(value as u64);
        TaskRunStats {
            runtime_ms,
            turns: self.turns,
            tool_calls: self.tool_calls,
            output_tokens: positive(self.output_tokens),
            total_tokens: positive(self.total_tokens),
            generation_ms: (self.generation_ms > 0).then_some(self.generation_ms),
            tokens_per_second: throughput_window_ms
                .and_then(|window| tokens_per_second(self.output_tokens, window as f64)),
            cost_usd: (self.saw_cost && self.cost_usd.is_finite()).then_some(self.cost_usd),
            cache_hit_rate_last: self.latest_cache_hit_rate,
            cache_hit_rate_run: bounded_cache_hit_rate(
                self.cache_read_tokens,
                self.cacheable_tokens,
            ),
        }
    }
}

/// Output tokens per second, rounded to an integer at 10+ and to one decimal below.
pub fn tokens_per_second(output_tokens: f64, generation_ms: f64) -> Option<f64> {
    if output_tokens <= 0.0 || generation_ms <= 0.0 {
        return None;
    }
    let raw = output_tokens / (generation_ms / 1_000.0);
    Some(if raw >= 10.0 {
        js_round(raw)
    } else {
        js_round(raw * 10.0) / 10.0
    })
}

/// `Math.round`: halves round toward positive infinity.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn is_nested_tool_call(call: &Value) -> bool {
    call.as_object().is_some_and(|call| {
        call.get("name")
            .and_then(Value::as_str)
            .is_some_and(|name| !name.is_empty())
            && call.get("ok").is_some_and(Value::is_boolean)
    })
}

fn bounded_cache_hit_rate(cache_read_tokens: f64, cacheable_tokens: f64) -> Option<f64> {
    if !cache_read_tokens.is_finite() || !cacheable_tokens.is_finite() || cacheable_tokens <= 0.0 {
        return None;
    }
    let rate = cache_read_tokens / cacheable_tokens;
    rate.is_finite().then(|| rate.clamp(0.0, 1.0))
}

fn assistant_message(message: Option<&Value>) -> Option<&Map<String, Value>> {
    message
        .and_then(Value::as_object)
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
}

#[derive(Default)]
struct UsageFacts {
    output: Option<f64>,
    total: Option<f64>,
    cost: Option<f64>,
    input: Option<f64>,
    cache_read: Option<f64>,
    cache_write: Option<f64>,
}

fn read_usage(message: &Map<String, Value>) -> UsageFacts {
    let Some(usage) = message.get("usage").and_then(Value::as_object) else {
        return UsageFacts::default();
    };
    let first = |keys: [&str; 2]| first_finite_non_negative(keys.map(|key| usage.get(key)));
    // Senpi reports cost either as a plain number or as a per-bucket breakdown carrying `.total`.
    let cost = usage.get("cost").and_then(|cost| match cost {
        Value::Object(breakdown) => first_finite_non_negative([breakdown.get("total")]),
        other => first_finite_non_negative([Some(other)]),
    });
    UsageFacts {
        output: first(["output", "output_tokens"]),
        total: first(["totalTokens", "total_tokens"]),
        cost,
        input: first(["input", "input_tokens"]),
        cache_read: first(["cacheRead", "cache_read_input_tokens"]),
        cache_write: first(["cacheWrite", "cache_creation_input_tokens"]),
    }
}

fn first_finite_non_negative<const N: usize>(values: [Option<&Value>; N]) -> Option<f64> {
    values
        .into_iter()
        .flatten()
        .filter_map(Value::as_f64)
        .find(|value| value.is_finite() && *value >= 0.0)
}

#[cfg(test)]
#[path = "run_stats_tests.rs"]
mod tests;
