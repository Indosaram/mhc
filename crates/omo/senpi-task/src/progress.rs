use serde::Serialize;
use serde_json::Value;

use crate::run_stats::RunStatsTracker;
use crate::run_stats::create_run_stats_tracker;
use crate::shared::ManagedChildEvent;
use crate::state::ResolvedModelRecord;
use crate::state::TaskRunStats;
use crate::status_line::StatusLineInput;
use crate::status_line::StatusLineStats;
use crate::status_line::StatusTargetInput;
use crate::status_line::TaskIdentityInput;
use crate::status_line::compose_status_line;
use crate::status_line::format_status_target;
use crate::status_line::task_identity_label;

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressActivity {
    pub activity: String,
    pub started_at: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolProgressDetails {
    pub progress: ProgressActivity,
    pub child_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_tool: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_assistant_line: Option<String>,
    pub turns: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_turns: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_per_second: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct ChildProgressTarget {
    pub category: Option<String>,
    pub agent_type: Option<String>,
    pub resolved_model: Option<ResolvedModelRecord>,
    pub model: Option<String>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub task_summary: Option<String>,
}

pub struct ChildProgress<C: Fn() -> u64> {
    task_id: String,
    // Name, description, summary and id never change after creation, so the label is built once.
    identity: String,
    target: ChildProgressTarget,
    started_at: u64,
    now: C,
    tracker: RunStatsTracker<C>,
    current_tool: Option<String>,
    last_assistant_line: Option<String>,
    last_total_tokens: Option<f64>,
    fallback_count: u64,
    turn_resumed: bool,
    awaiting_input: bool,
}

pub fn create_child_progress<C: Fn() -> u64 + Clone>(
    task_id: &str,
    target: ChildProgressTarget,
    started_at: u64,
    now: C,
) -> ChildProgress<C> {
    let identity = task_identity_label(&TaskIdentityInput {
        task_id,
        name: target.name.as_deref(),
        description: target.description.as_deref(),
        task_summary: target.task_summary.as_deref(),
    });
    ChildProgress {
        task_id: task_id.to_string(),
        identity,
        target,
        started_at,
        tracker: create_run_stats_tracker(started_at, now.clone()),
        now,
        current_tool: None,
        last_assistant_line: None,
        last_total_tokens: None,
        fallback_count: 0,
        turn_resumed: false,
        awaiting_input: false,
    }
}

impl<C: Fn() -> u64> ChildProgress<C> {
    pub fn accept(&mut self, event: &ManagedChildEvent) -> bool {
        let stats_changed = self.tracker.accept(event);
        match event.event_type.as_str() {
            "host_turn_resumed" => {
                self.turn_resumed = true;
                return true;
            }
            "task_awaiting_input" => {
                self.awaiting_input = true;
                return true;
            }
            _ => {}
        }
        match event.event_type.as_str() {
            "retry_fallback_applied" => {
                let Some(to) = &event.to else {
                    return stats_changed;
                };
                self.target.resolved_model = None;
                self.target.model = Some(to.clone());
                self.fallback_count += 1;
                true
            }
            "tool_execution_start" => {
                let Some(tool_name) = &event.tool_name else {
                    return stats_changed;
                };
                let args = event.args.as_ref().or(event.input.as_ref());
                self.current_tool = Some(format_tool_activity(tool_name, args));
                true
            }
            "tool_execution_end" => {
                if self.current_tool.take().is_none() {
                    return stats_changed;
                }
                true
            }
            "message_end" => {
                let Some(line) = assistant_last_line(event.message.as_ref()) else {
                    return stats_changed;
                };
                self.last_assistant_line = Some(line);
                if let Some(tokens) = read_tokens(event.message.as_ref()) {
                    self.last_total_tokens = Some(tokens);
                }
                true
            }
            _ => stats_changed,
        }
    }

    pub fn details(&self) -> ToolProgressDetails {
        let stats = self.tracker.snapshot((self.now)());
        ToolProgressDetails {
            progress: ProgressActivity {
                activity: self.activity(&stats),
                started_at: self.started_at as f64,
            },
            child_id: self.task_id.clone(),
            current_tool: self.current_tool.clone(),
            last_assistant_line: self.last_assistant_line.clone(),
            turns: stats.turns as f64,
            failed_turns: stats.failed_turns.map(|turns| turns as f64),
            tool_calls: Some(stats.tool_calls as f64),
            tokens: self.last_total_tokens,
            output_tokens: stats.output_tokens.map(|tokens| tokens as f64),
            tokens_per_second: stats.tokens_per_second,
        }
    }

    // The partial-result content row. The composed status line intentionally lives ONLY in
    // details.progress.activity: senpi's ToolExecutionRenderer already draws that line below the
    // result, so echoing it in content would render the same status twice.
    pub fn content_text(&self) -> String {
        self.last_assistant_line
            .as_ref()
            .map_or_else(String::new, |line| format!("↳ last: {line}"))
    }

    fn activity(&self, stats: &TaskRunStats) -> String {
        let target = &self.target;
        let status_target = format_status_target(&StatusTargetInput {
            category: target.category.as_deref(),
            agent_type: target.agent_type.as_deref(),
            resolved_model: target.resolved_model.as_ref(),
            model: target.model.as_deref(),
            fallback_count: Some(self.fallback_count),
        });
        let line_stats = StatusLineStats {
            turns: stats.turns,
            failed_turns: stats.failed_turns.unwrap_or_default(),
            tool_calls: stats.tool_calls,
            runtime_ms: Some(stats.runtime_ms),
            tokens_per_second: stats.tokens_per_second,
            cost_usd: stats.cost_usd,
            cache_hit_rate_last: stats.cache_hit_rate_last,
            cache_hit_rate_run: stats.cache_hit_rate_run,
        };
        let verb = select_live_activity_verb(
            self.current_tool.as_deref(),
            stats.turns,
            stats.failed_turns.unwrap_or_default(),
            self.turn_resumed,
            self.awaiting_input,
        );
        compose_status_line(&StatusLineInput {
            identity: &self.identity,
            target: status_target.as_deref(),
            stats: Some(&line_stats),
            verb: Some(&verb),
        })
    }
}

pub fn select_live_activity_verb(
    tool: Option<&str>,
    turns: u64,
    failed_turns: u64,
    turn_resumed: bool,
    awaiting_input: bool,
) -> String {
    if let Some(tool) = tool {
        format!("running {tool}")
    } else if turns > 0 || turn_resumed {
        "running".to_string()
    } else if awaiting_input {
        "resumed, waiting for input".to_string()
    } else if failed_turns > 0 {
        "retrying".to_string()
    } else {
        "starting".to_string()
    }
}

pub fn read_tool_progress_details(value: &Value) -> Option<ToolProgressDetails> {
    let record = value.as_object()?;
    let progress = record.get("progress")?.as_object()?;
    let activity = progress.get("activity")?.as_str()?.to_string();
    let started_at = progress.get("startedAt")?.as_f64()?;
    let child_id = record.get("childId")?.as_str()?.to_string();
    let turns = record.get("turns")?.as_f64()?;
    let optional_string = |key: &str| -> Result<Option<String>, ()> {
        match record.get(key) {
            None => Ok(None),
            Some(Value::String(text)) => Ok(Some(text.clone())),
            Some(_) => Err(()),
        }
    };
    let optional_number = |key: &str| -> Result<Option<f64>, ()> {
        match record.get(key) {
            None => Ok(None),
            Some(Value::Number(number)) => number.as_f64().map(Some).ok_or(()),
            Some(_) => Err(()),
        }
    };
    Some(ToolProgressDetails {
        progress: ProgressActivity {
            activity,
            started_at,
        },
        child_id,
        current_tool: optional_string("currentTool").ok()?,
        last_assistant_line: optional_string("lastAssistantLine").ok()?,
        turns,
        failed_turns: optional_number("failedTurns").ok()?,
        tool_calls: optional_number("toolCalls").ok()?,
        tokens: optional_number("tokens").ok()?,
        output_tokens: optional_number("outputTokens").ok()?,
        tokens_per_second: optional_number("tokensPerSecond").ok()?,
    })
}

// The `running <tool>` fragment of the live status line, e.g. `read src/foo.ts`.
pub fn format_tool_activity(tool_name: &str, args: Option<&Value>) -> String {
    let argument = one_line_argument(args);
    if argument.is_empty() {
        tool_name.to_string()
    } else {
        format!("{tool_name} {argument}")
    }
}

// The last non-empty line of an assistant message, bounded for a single status row.
pub fn assistant_last_line(message: Option<&Value>) -> Option<String> {
    let text = assistant_text(message?)?;
    Some(truncate(&last_non_empty_line(&text), 120))
}

fn one_line_argument(value: Option<&Value>) -> String {
    let text = match value {
        Some(Value::String(text)) => Some(text.as_str()),
        Some(Value::Object(record)) => record.values().find_map(Value::as_str),
        _ => None,
    };
    text.map_or_else(String::new, |text| {
        truncate(&text.split_whitespace().collect::<Vec<_>>().join(" "), 80)
    })
}

fn assistant_text(message: &Value) -> Option<String> {
    if message.get("role")?.as_str()? != "assistant" {
        return None;
    }
    let text: String = message
        .get("content")?
        .as_array()?
        .iter()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    (!text.is_empty()).then_some(text)
}

fn read_tokens(message: Option<&Value>) -> Option<f64> {
    let usage = message?.get("usage")?.as_object()?;
    ["totalTokens", "total_tokens"]
        .iter()
        .find_map(|key| usage.get(*key).and_then(Value::as_f64))
}

fn last_non_empty_line(text: &str) -> String {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .rfind(|line| !line.trim().is_empty())
        .map_or_else(String::new, |line| line.trim().to_string())
}

fn truncate(text: &str, maximum: usize) -> String {
    // UTF-16 units never exceed UTF-8 bytes, so a short byte length skips the unit scan.
    if text.len() <= maximum {
        return text.to_string();
    }
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= maximum {
        return text.to_string();
    }
    format!("{}…", String::from_utf16_lossy(&units[..maximum - 1]))
}

#[cfg(test)]
#[path = "progress_tests.rs"]
mod tests;
