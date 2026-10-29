//! `tools/run-stats-format.ts`: run-stats suffix and result tokens shared by the tool renderers.

use crate::state::TaskRunStats;
use crate::status_line::{format_cache_hit_percent, format_cost_usd, format_run_spend, js_number_text};

pub fn format_run_duration(duration_ms: u64) -> String {
    // `Math.round(ms / 1000)` rounds half up for the non-negative inputs a duration can take.
    let total_seconds = duration_ms.saturating_add(500) / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        return format!("{hours}h {minutes}m");
    }
    if minutes > 0 {
        return format!("{minutes}m {seconds}s");
    }
    format!("{seconds}s")
}

/// Prose-style suffix for the task_output status row:
/// ` · ran 2m 14s · 5 tools · $0.4213 (CH: 87%) · 118 tok/s`.
pub fn run_stats_suffix(stats: Option<&TaskRunStats>) -> String {
    let Some(stats) = stats else {
        return String::new();
    };
    let mut parts = vec![
        format!("ran {}", format_run_duration(stats.runtime_ms)),
        format!("{} {}", stats.tool_calls, if stats.tool_calls == 1 { "tool" } else { "tools" }),
    ];
    if let Some(spend) = format_run_spend(stats.cost_usd, stats.cache_hit_rate_run) {
        parts.push(spend);
    }
    if let Some(tps) = stats.tokens_per_second {
        parts.push(format!("{} tok/s", js_number_text(tps)));
    }
    parts.iter().map(|part| format!(" \u{b7} {part}")).collect()
}

/// Token-style fragments for the task result row: `ran:2m14s tools:5 cost:$0.4213 ch:87% tps:118`.
pub fn run_stats_result_tokens(stats: Option<&TaskRunStats>) -> Vec<String> {
    let Some(stats) = stats else {
        return Vec::new();
    };
    let duration = format_run_duration(stats.runtime_ms).replace(' ', "");
    let cost = if stats.cost_usd == Some(0.0) { None } else { format_cost_usd(stats.cost_usd) };
    let cache_hit = format_cache_hit_percent(stats.cache_hit_rate_run);
    let mut tokens = vec![format!("ran:{duration}"), format!("tools:{}", stats.tool_calls)];
    if let Some(cost) = cost {
        tokens.push(format!("cost:{cost}"));
    }
    if let Some(cache_hit) = cache_hit {
        // `"(CH: 87%)".slice(5, -1)` -> `87%`.
        tokens.push(format!("ch:{}", &cache_hit[5..cache_hit.len() - 1]));
    }
    if let Some(tps) = stats.tokens_per_second {
        tokens.push(format!("tps:{}", js_number_text(tps)));
    }
    tokens
}
