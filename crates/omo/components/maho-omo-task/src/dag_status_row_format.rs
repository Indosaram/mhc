use std::collections::BTreeMap;
use senpi_task::{dag::types::{DagRoute, DagRunSnapshot}, renderer_text::{excerpt_renderer_text, normalize_renderer_text}};

fn icon(state: &str) -> &str { match state { "running" => "▶", "completed" => "✓", "failed" => "✗", "skipped" | "cancelled" => "⊘", "paused" => "⏸", _ => "○" } }
pub fn route_label(route: &DagRoute) -> String {
    match route {
        DagRoute::Category { category } => format!("category:{}", normalize_renderer_text(category)),
        DagRoute::Agent { agent, model } => {
            let agent = normalize_renderer_text(agent);
            model.as_ref().map_or_else(|| format!("agent:{agent}"), |model| format!("agent:{agent}({})", normalize_renderer_text(model)))
        }
    }
}
pub fn run_rows(run: &DagRunSnapshot, activity: Option<&BTreeMap<String, String>>) -> Vec<String> {
    let total = run.waves.len();
    let current = run.waves.iter().position(|wave| wave.node_ids.iter().any(|id| run.nodes.iter().find(|node| &node.id == id).is_none_or(|node| !node.state.is_terminal()))).map_or(total, |index| index + 1);
    let completed = run.nodes.iter().filter(|node| node.state.as_str() == "completed").count();
    let running = run.nodes.iter().filter(|node| node.state.as_str() == "running").count();
    let failed = run.nodes.iter().filter(|node| node.state.as_str() == "failed").count();
    let mut counts = vec![format!("{completed}/{} done", run.nodes.len())];
    if running > 0 { counts.push(format!("{running} running")); }
    if failed > 0 { counts.push(format!("{failed} failed")); }
    let mut rows = vec![format!("{} {} {} wave {current}/{total} {}", icon(run.status.as_str()), excerpt_renderer_text(&run.name, Some(32)), run.status.as_str(), counts.join(", "))];
    for node in run.nodes.iter().take(12) {
        let mut parts = vec![format!("  {}", icon(node.state.as_str())), excerpt_renderer_text(node.label.as_deref().unwrap_or(&node.id), Some(32)), route_label(&node.route)];
        if node.state.as_str() == "running" && let Some(live) = activity.and_then(|activity| activity.get(&node.id)) { parts.push(excerpt_renderer_text(live, Some(40))); }
        rows.push(parts.join(" "));
    }
    if run.nodes.len() > 12 { rows.push(format!("  +{} more", run.nodes.len() - 12)); }
    rows
}
