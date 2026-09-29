use std::path::Path;

use chrono::DateTime;

use super::types::{COMPLETION_CUSTOM_TYPE, CompletionDetails, ParentNotifierMessage};
use crate::renderer_text::{
    excerpt_renderer_prompt_text, join_renderer_tokens, normalize_renderer_text,
    renderer_visible_width,
};
use crate::shared::{format_run_duration, utf16_prefix};
use crate::state::{Messageability, TaskRecord, messageability};
use crate::status_line::{StatusTargetInput, format_target_with_model, js_number_text};

pub const FINAL_RESPONSE_TRANSPORT_LIMIT: usize = 32_000;

#[derive(Debug, Clone, Copy, Default)]
pub struct BuildDetailsOptions<'a> {
    pub tokens: Option<u64>,
    pub state_dir: Option<&'a Path>,
}

pub fn build_completion_details(
    record: &TaskRecord,
    options: BuildDetailsOptions<'_>,
) -> CompletionDetails {
    let (final_response, final_response_file) =
        final_response_for_notification(record, options.state_dir);
    let run_stats = record.run_stats.clone();
    let tokens = options
        .tokens
        .or_else(|| run_stats.as_ref().and_then(|stats| stats.total_tokens));
    CompletionDetails {
        task_id: record.task_id.clone(),
        name: record
            .name
            .clone()
            .unwrap_or_else(|| record.task_id.clone()),
        status: record.status,
        category: record.category.clone(),
        agent_type: record.agent_type.clone(),
        model: record.model.clone(),
        requested_model: record.requested_model.clone(),
        fallback_models: record.fallback_models.clone(),
        resolved_model: record.resolved_model.clone(),
        duration_ms: duration_ms(record),
        tokens,
        run_stats,
        final_response,
        final_response_file,
        continuation_hint: continuation_hint(record),
    }
}

pub fn build_completion_message(details: &[CompletionDetails]) -> ParentNotifierMessage {
    ParentNotifierMessage {
        custom_type: COMPLETION_CUSTOM_TYPE,
        content: completion_message_lines(details, None).join("\n"),
        display: false,
        details: details.to_vec(),
        trigger_turn: None,
    }
}

pub fn completion_message_lines(
    details: &[CompletionDetails],
    width: Option<usize>,
) -> Vec<String> {
    details
        .iter()
        .flat_map(|detail| completion_detail_lines(detail, width))
        .collect()
}

/// Spill failures fall back to the truncated inline text; the full result stays in the record.
fn final_response_for_notification(
    record: &TaskRecord,
    state_dir: Option<&Path>,
) -> (String, Option<String>) {
    let source = record
        .final_response
        .as_deref()
        .or(record.error_message.as_deref())
        .unwrap_or("");
    if source.encode_utf16().count() <= FINAL_RESPONSE_TRANSPORT_LIMIT {
        return (source.to_string(), None);
    }
    let text = utf16_prefix(source, FINAL_RESPONSE_TRANSPORT_LIMIT).to_string();
    let Some(state_dir) = state_dir else {
        return (text, None);
    };
    let dir = state_dir.join("completion-results");
    let path = dir.join(format!("{}.txt", record.task_id));
    match std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, source)) {
        Ok(()) => (text, Some(format!("local://{}", path.display()))),
        Err(error) => {
            utils::logger::log(
                "senpi-task completion spill failed",
                Some(&serde_json::json!({ "taskId": record.task_id, "error": error.to_string() })),
            );
            (text, None)
        }
    }
}

fn duration_ms(record: &TaskRecord) -> i64 {
    match (
        DateTime::parse_from_rfc3339(&record.created_at),
        DateTime::parse_from_rfc3339(&record.updated_at),
    ) {
        (Ok(started), Ok(ended)) => (ended - started).num_milliseconds().max(0),
        _ => 0,
    }
}

fn continuation_hint(record: &TaskRecord) -> String {
    match messageability(record.status, record.residency_state) {
        Messageability::NotContinuable => String::new(),
        Messageability::Steer | Messageability::Revive => format!(
            "Use task_send({{ to: \"{}\", message: \"...\" }}) to continue.",
            record.task_id
        ),
    }
}

fn completion_detail_lines(detail: &CompletionDetails, width: Option<usize>) -> Vec<String> {
    let name = format!("name:{}", normalize_renderer_text(&detail.name));
    let id = format!("id:{}", normalize_renderer_text(&detail.task_id));
    let target = format_target_with_model(&StatusTargetInput {
        category: detail.category.as_deref(),
        agent_type: detail.agent_type.as_deref(),
        resolved_model: detail.resolved_model.as_ref(),
        model: Some(&detail.model),
        fallback_count: None,
    });
    let fallback = fallback_token(detail);
    let status = format!("status:{}", detail.status.as_str());
    let identity = join_renderer_tokens(&[
        Some("task completion"),
        Some(&name),
        Some(&id),
        target.as_deref(),
        fallback.as_deref(),
        Some(&status),
    ]);
    let duration = format!("duration:{}", format_duration(detail.duration_ms));
    let tokens = detail.tokens.map(|tokens| format!("tokens:{tokens}"));
    let tools = detail
        .run_stats
        .as_ref()
        .map(|stats| format!("tools:{}", stats.tool_calls));
    let tps = detail
        .run_stats
        .as_ref()
        .and_then(|stats| stats.tokens_per_second)
        .map(|tps| format!("tps:{}", js_number_text(tps)));
    let stats = join_renderer_tokens(&[
        Some(&duration),
        tokens.as_deref(),
        tools.as_deref(),
        tps.as_deref(),
    ]);
    let mut lines = match width {
        None => vec![join_renderer_tokens(&[Some(&identity), Some(&stats)])],
        Some(_) => vec![
            excerpt_renderer_prompt_text(&identity, width),
            excerpt_renderer_prompt_text(&stats, width),
        ],
    };
    let (response, continuation) = match width {
        None => (
            detail.final_response.clone(),
            detail.continuation_hint.clone(),
        ),
        Some(_) => (
            normalize_renderer_text(&detail.final_response),
            normalize_renderer_text(&detail.continuation_hint),
        ),
    };
    if !response.is_empty() {
        let excerpt = excerpt_for_width(&response, width, "result:\"", "\"");
        lines.push(format!("result:\"{excerpt}\""));
    }
    if let Some(file) = &detail.final_response_file {
        lines.push(format!(
            "result_file:{}",
            excerpt_for_width(file, width, "result_file:", "")
        ));
    }
    if !continuation.is_empty() {
        lines.push(format!(
            "next:{}",
            excerpt_for_width(&continuation, width, "next:", "")
        ));
    }
    lines
}

fn fallback_token(detail: &CompletionDetails) -> Option<String> {
    let requested = &detail.requested_model.as_ref()?.display;
    let resolved = detail
        .resolved_model
        .as_ref()
        .map_or(detail.model.as_str(), |model| model.display.as_str());
    (requested != resolved).then(|| {
        format!(
            "fallback:{}->{}",
            normalize_renderer_text(requested),
            normalize_renderer_text(resolved)
        )
    })
}

fn excerpt_for_width(value: &str, width: Option<usize>, prefix: &str, suffix: &str) -> String {
    match width {
        None => value.to_string(),
        Some(width) => {
            let available = width
                .saturating_sub(renderer_visible_width(prefix))
                .saturating_sub(renderer_visible_width(suffix));
            excerpt_renderer_prompt_text(value, Some(available))
        }
    }
}

fn format_duration(duration_ms: i64) -> String {
    if duration_ms < 1_000 {
        return format!("{duration_ms}ms");
    }
    if duration_ms >= 60_000 {
        return format_run_duration(duration_ms);
    }
    let fixed = format!("{:.2}", duration_ms as f64 / 1_000.0);
    let trimmed = match fixed.strip_suffix(".00") {
        Some(whole) => whole.to_string(),
        None if fixed.ends_with('0') => fixed[..fixed.len() - 1].to_string(),
        None => fixed,
    };
    format!("{trimmed}s")
}
