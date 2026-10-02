use std::sync::Arc;
use maho_ext_api::{ExtensionApi, ExtensionFailure, NotificationType};
use senpi_task::{dag::{manager::{DagManager, DagRunSummary}, types::{DagRoute, DagRunSnapshot}}, state::TaskRecord, renderer_text::normalize_renderer_text};

pub trait DagCommandManager: Send + Sync {
    fn list(&self, session: &str, limit: usize) -> Result<Vec<DagRunSummary>, ExtensionFailure>;
    fn snapshot(&self, run: &str, session: &str) -> Option<DagRunSnapshot>;
    fn task_record(&self, task: &str) -> Option<TaskRecord>;
}
pub struct DagCommandAdapter {
    pub manager: DagManager,
    pub tasks: senpi_task::manager::TaskManager,
}
impl DagCommandManager for DagCommandAdapter {
    fn list(&self, session: &str, limit: usize) -> Result<Vec<DagRunSummary>, ExtensionFailure> { self.manager.list(session, Some(limit)).map_err(|error| ExtensionFailure::new(error.to_string())) }
    fn snapshot(&self, run: &str, session: &str) -> Option<DagRunSnapshot> { self.manager.snapshot(&run.to_owned(), session).ok() }
    fn task_record(&self, task: &str) -> Option<TaskRecord> { self.tasks.get(task) }
}
pub fn register_dag_commands(api: &mut ExtensionApi, manager: Arc<dyn DagCommandManager>) {
    api.register_command("dag", Some("List dag runs, or show one run's wave tree.".into()), None, Arc::new(move |args, ctx| {
        let manager = manager.clone();
        Box::pin(async move {
            let (text, kind) = dag_command_text(manager.as_ref(), args, Some(ctx.session_manager.session_id()))?;
            ctx.ui.notify(&text, kind);
            Ok(())
        })
    }));
}
pub fn dag_command_text(manager: &dyn DagCommandManager, args: &str, session: Option<&str>) -> Result<(String, NotificationType), ExtensionFailure> {
    let Some(session) = session else { return Ok(("No dag runs in this session.".into(), NotificationType::Info)) };
    if let Some(run) = args.split_whitespace().next() {
        let Some(snapshot) = manager.snapshot(run, session) else { return Ok((format!("No dag run \"{}\" in this session.", normalize_renderer_text(run)), NotificationType::Warning)) };
        return Ok((detail_text(&snapshot, manager).join("\n"), NotificationType::Info));
    }
    let runs = manager.list(session, 20)?;
    let rows: Vec<_> = runs.iter().map(|run| {
        let mut counts = vec![format!("{}/{} done", run.counts.completed, run.counts.total)];
        if run.counts.running > 0 { counts.push(format!("{} running", run.counts.running)); }
        if run.counts.failed > 0 { counts.push(format!("{} failed", run.counts.failed)); }
        format!("{} ({}) {} {}", normalize_renderer_text(&run.name), normalize_renderer_text(&run.run_id), run.status.as_str(), counts.join(", "))
    }).collect();
    Ok((if rows.is_empty() { "No dag runs in this session.".into() } else { rows.join("\n") }, NotificationType::Info))
}
pub fn detail_text(run: &DagRunSnapshot, manager: &dyn DagCommandManager) -> Vec<String> {
    let mut rows = vec![format!("{} ({}) {} {} nodes, {} waves", normalize_renderer_text(&run.name), normalize_renderer_text(&run.run_id), run.status.as_str(), run.nodes.len(), run.waves.len())];
    for (position, wave) in run.waves.iter().enumerate() {
        rows.push(format!("wave {}/{}", position + 1, run.waves.len()));
        for id in &wave.node_ids {
            let Some(node) = run.nodes.iter().find(|node| &node.id == id) else { rows.push(format!("  {} missing", normalize_renderer_text(id))); continue };
            let route = match &node.route { DagRoute::Category { category } => format!("category:{}", normalize_renderer_text(category)), DagRoute::Agent { agent, .. } => format!("agent:{}", normalize_renderer_text(agent)) };
            let mut parts = vec![normalize_renderer_text(node.label.as_deref().unwrap_or(id)), node.state.as_str().into(), route];
            let record = node.task_id.as_deref().and_then(|id| manager.task_record(id));
            let model = record.as_ref().map(|record| record.resolved_model.as_ref().map_or(record.model.as_str(), |model| model.display.as_str())).or(match &node.route { DagRoute::Agent { model, .. } => model.as_deref(), DagRoute::Category { .. } => None });
            if let Some(model) = model { parts.push(format!("model:{}", normalize_renderer_text(model))); }
            if node.attempt > 1 { parts.push(format!("attempt {}", node.attempt)); }
            if let (Some(start), Some(end)) = (&node.started_at, &node.completed_at) && let (Ok(start), Ok(end)) = (chrono::DateTime::parse_from_rfc3339(start), chrono::DateTime::parse_from_rfc3339(end)) && end >= start {
                let milliseconds = (end - start).num_milliseconds();
                let tenths = milliseconds.saturating_add(50) / 100;
                parts.push(format!("{}.{:01}s", tenths / 10, tenths % 10));
            }
            let mut dependencies = node.depends_on.clone();
            for edge in run.edges.iter().filter(|edge| edge.to == node.id) { if !dependencies.contains(&edge.from) { dependencies.push(edge.from.clone()); } }
            if !dependencies.is_empty() { parts.push(format!("after {}", dependencies.iter().map(|id| normalize_renderer_text(id)).collect::<Vec<_>>().join(", "))); }
            if run.critical_path.contains(id) { parts.push("*critical*".into()); }
            if let Some(error) = &node.error { parts.push(format!("error: {}", normalize_renderer_text(&error.message))); }
            rows.push(format!("  {}", parts.join(" ")));
        }
    }
    if !run.critical_path.is_empty() { rows.push(format!("critical path: {}", run.critical_path.iter().map(|id| normalize_renderer_text(id)).collect::<Vec<_>>().join(" -> "))); }
    for bottleneck in &run.bottlenecks { rows.push(format!("bottleneck: {} blocks {}", normalize_renderer_text(&bottleneck.node_id), bottleneck.blocked_count)); }
    rows
}
