use crate::renderer_text::excerpt_renderer_text;
use crate::renderer_text::optional_renderer_text;
use crate::state::ResolvedModelRecord;

// One status row must survive next to a spinner, elapsed clock and the terminal edge, so the human
// label is bounded here rather than at each call site.
const IDENTITY_MAX_WIDTH: usize = 48;

#[derive(Debug, Clone, Default)]
pub struct TaskIdentityInput<'a> {
    pub task_id: &'a str,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    pub task_summary: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub struct StatusTargetInput<'a> {
    pub category: Option<&'a str>,
    pub agent_type: Option<&'a str>,
    pub resolved_model: Option<&'a ResolvedModelRecord>,
    // Raw model string used when no resolved model metadata exists (live/legacy tasks).
    pub model: Option<&'a str>,
    pub fallback_count: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StatusLineStats {
    pub turns: u64,
    pub tool_calls: u64,
    pub runtime_ms: Option<u64>,
    pub tokens_per_second: Option<f64>,
    pub cost_usd: Option<f64>,
    pub cache_hit_rate_last: Option<f64>,
    pub cache_hit_rate_run: Option<f64>,
}

#[derive(Debug, Clone, Default)]
pub struct StatusLineInput<'a> {
    pub identity: &'a str,
    pub target: Option<&'a str>,
    pub stats: Option<&'a StatusLineStats>,
    pub verb: Option<&'a str>,
}

// WHAT the task is, not which opaque handle it got: the delegated-work summary beats the
// description (human label), which beats the generated name; the task id is only the last-resort
// handle when none was supplied.
pub fn task_identity_label(input: &TaskIdentityInput<'_>) -> String {
    let label = optional_renderer_text(input.task_summary)
        .or_else(|| optional_renderer_text(input.description))
        .or_else(|| optional_renderer_text(input.name));
    let width = Some(IDENTITY_MAX_WIDTH);
    match label {
        Some(label) => excerpt_renderer_text(&label, width),
        None => excerpt_renderer_text(input.task_id, width),
    }
}

// WHO it runs as: one routing identity, shared by category- and agent-routed tasks. Category wins
// when both are present (a record never carries both, but a defensive caller might).
pub fn format_target_identity(category: Option<&str>, agent_type: Option<&str>) -> Option<String> {
    if let Some(category) = optional_renderer_text(category) {
        return Some(format!("category:{category}"));
    }
    optional_renderer_text(agent_type).map(|agent_type| format!("agent:{agent_type}"))
}

// WHO it runs as plus WHICH model it resolved to, in the single canonical grammar:
//   category:<name>(<provider>/<model>:<effort>) | agent:<name>(<provider>/<model>:<effort>)
// Agent and category targets share the exact same shape; without an identity the bare model token
// is the only useful signal left.
pub fn format_target_with_model(input: &StatusTargetInput<'_>) -> Option<String> {
    let identity = format_target_identity(input.category, input.agent_type);
    let model =
        format_status_model(input.resolved_model).or_else(|| optional_renderer_text(input.model));
    match (identity, model) {
        (Some(identity), Some(model)) => Some(format!("{identity}({model})")),
        (Some(identity), None) => Some(identity),
        (None, Some(model)) => Some(format!("model:{model}")),
        (None, None) => None,
    }
}

// WHERE it runs: the routing target plus the model actually resolved for it.
pub fn format_status_target(input: &StatusTargetInput<'_>) -> Option<String> {
    let target = format_target_with_model(input)?;
    match input.fallback_count {
        Some(count) if count > 0 => Some(format!("{target} · fallback:{count}")),
        Some(_) | None => Some(target),
    }
}

// The canonical grammar every live/status row shares:
//   <identity> · <target (model)> · turn N (M tools) · <verb> · $C · T tok/s
pub fn compose_status_line(input: &StatusLineInput<'_>) -> String {
    let stats = input.stats;
    let tokens = [
        Some(input.identity.to_string()),
        input.target.map(str::to_string),
        stats.map(|stats| {
            format!(
                "turn {}{}",
                stats.turns,
                tool_count_suffix(stats.tool_calls)
            )
        }),
        input.verb.map(str::to_string),
        stats.and_then(|stats| format_live_spend(stats.cost_usd)),
        stats
            .and_then(|stats| stats.tokens_per_second)
            .map(|tps| format!("{} tok/s", js_number_text(tps))),
    ];
    tokens
        .into_iter()
        .flatten()
        .filter(|token| !token.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

// Running task rows keep spend compact; cache-hit rate remains available in completed-run details.
pub fn format_live_spend(cost_usd: Option<f64>) -> Option<String> {
    format_cost_usd(cost_usd)
}

// Completed-run summaries pair spend with the cumulative whole-run cache-hit rate.
pub fn format_run_spend(cost_usd: Option<f64>, cache_hit_rate_run: Option<f64>) -> Option<String> {
    let cost = format_cost_usd(cost_usd);
    let cache_hit = format_cache_hit_percent(cache_hit_rate_run);
    match (cost, cache_hit) {
        (Some(cost), Some(cache_hit)) => Some(format!("{cost} {cache_hit}")),
        (Some(cost), None) => Some(cost),
        (None, cache_hit) => cache_hit,
    }
}

pub fn format_cost_usd(cost_usd: Option<f64>) -> Option<String> {
    let cost = cost_usd.filter(|cost| cost.is_finite() && *cost >= 0.0)?;
    Some(format!("${cost:.4}"))
}

pub fn format_cache_hit_percent(cache_hit_rate: Option<f64>) -> Option<String> {
    let rate = cache_hit_rate.filter(|rate| rate.is_finite() && (0.0..=1.0).contains(rate))?;
    Some(format!("(CH: {}%)", (rate * 100.0 + 0.5).floor()))
}

pub fn tool_count_suffix(tool_calls: u64) -> String {
    match tool_calls {
        0 => String::new(),
        1 => " (1 tool)".to_string(),
        count => format!(" ({count} tools)"),
    }
}

/// Renders a float the way JS template literals do (`62`, not `62.0`).
pub(crate) fn js_number_text(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn format_status_model(resolved: Option<&ResolvedModelRecord>) -> Option<String> {
    let resolved = resolved?;
    let provider = optional_renderer_text(Some(&resolved.provider))?;
    let model_id = optional_renderer_text(Some(&resolved.model_id))?;
    let effort = optional_renderer_text(resolved.reasoning.as_deref())
        .or_else(|| optional_renderer_text(resolved.reasoning_effort.as_deref()))
        .or_else(|| optional_renderer_text(resolved.variant.as_deref()));
    Some(match effort {
        Some(effort) => format!("{provider}/{model_id}:{effort}"),
        None => format!("{provider}/{model_id}"),
    })
}

#[cfg(test)]
#[path = "status_line_tests.rs"]
mod tests;
