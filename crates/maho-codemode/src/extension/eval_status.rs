use crate::tool::{detached_cell_contract::EvalDetachedCellStatusEntry, types::EvalLanguage};

pub const EVAL_CELLS_STATUS_KEY: &str = "eval-cells";

fn truncate_end(text: &str, max: usize) -> String {
    if text.encode_utf16().count() <= max { return text.into(); }
    let units = text.encode_utf16().take(max.saturating_sub(1)).collect::<Vec<_>>();
    format!("{}…", String::from_utf16_lossy(&units))
}

fn pack_labels(labels: &[String], budget: usize) -> String {
    for kept in (1..=labels.len()).rev() {
        let hidden = labels.len() - kept;
        let tail = if hidden > 0 { format!(" +{hidden} more") } else { String::new() };
        let joined = labels[..kept].join(", ");
        if joined.encode_utf16().count() + tail.len() <= budget { return joined + &tail; }
    }
    let tail = if labels.len() > 1 { format!(" +{} more", labels.len() - 1) } else { String::new() };
    truncate_end(labels.first().map_or("", String::as_str), budget.saturating_sub(tail.len()).max(1)) + &tail
}

pub fn format_elapsed_seconds(value: f64) -> String {
    let seconds = value.trunc().max(0.0);
    if seconds < 60.0 { return format!("{seconds}s"); }
    let minutes = (seconds / 60.0).trunc();
    if minutes < 60.0 { return format!("{minutes}m"); }
    let hours = (minutes / 60.0).trunc();
    let remaining = minutes % 60.0;
    if hours >= 24.0 { return format!("{}d {}h {remaining}m", (hours / 24.0).trunc(), hours % 24.0); }
    if remaining == 0.0 { format!("{hours}h") } else { format!("{hours}h {remaining}m") }
}

pub fn eval_cell_elapsed_seconds(entries: &[EvalDetachedCellStatusEntry], now_ms: f64) -> f64 {
    let oldest = entries.iter().filter(|entry| entry.queued_behind.is_none()).map(|entry| entry.started_at_ms).fold(f64::INFINITY, f64::min);
    if !oldest.is_finite() { return 0.0; }
    (((now_ms - oldest) / 1000.0) + 0.5).floor().max(0.0)
}

pub fn format_eval_cell_status(entries: &[EvalDetachedCellStatusEntry], now_ms: f64) -> Option<String> {
    let first = entries.first()?;
    let suffix = if entries.iter().all(|entry| entry.queued_behind.is_some()) { " (queued)".into() }
        else { format!(" ({})", format_elapsed_seconds(eval_cell_elapsed_seconds(entries, now_ms))) };
    let label = |entry: &EvalDetachedCellStatusEntry| {
        let text = entry.summary.as_deref().filter(|text| !text.is_empty()).unwrap_or(&entry.cell_id);
        if entry.queued_behind.is_some() { format!("queued {text}") } else { text.into() }
    };
    if entries.len() == 1 {
        let language = match first.language { EvalLanguage::Js => "js", EvalLanguage::Py => "py", EvalLanguage::Rb => "rb", EvalLanguage::Jl => "jl" };
        let head = format!("↗ {language} · ");
        return Some(format!("{head}{}{suffix}", truncate_end(&label(first), 48usize.saturating_sub(head.encode_utf16().count() + suffix.len()))));
    }
    let head = format!("↗ eval {}: ", entries.len());
    Some(format!("{head}{}{suffix}", pack_labels(&entries.iter().map(label).collect::<Vec<_>>(), 48usize.saturating_sub(head.encode_utf16().count() + suffix.len()))))
}
