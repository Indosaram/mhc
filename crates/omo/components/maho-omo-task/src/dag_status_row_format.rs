use std::{collections::BTreeMap, sync::Arc};
use senpi_task::{dag::{store::default_is_process_alive, types::{DagNode, DagNodeState, DagRoute, DagRunSnapshot}}, renderer_text::{excerpt_renderer_text, normalize_renderer_text, renderer_visible_width}};

const MAX_NODE_ROWS: usize = 12;
const LABEL_MIN: usize = 16;
const LABEL_MAX: usize = 32;
const ROUTE_MIN: usize = 12;
const ACTIVITY_MIN: usize = 8;
const NARROW_ROW_WIDTH: usize = 60;
const NODE_ROW_WIDTH_MAX: usize = 220;
const SEPARATOR: &str = " · ";
const TERMINAL: [DagNodeState; 4] = [DagNodeState::Completed, DagNodeState::Failed, DagNodeState::Cancelled, DagNodeState::Skipped];
fn icon(state: &str) -> &str { match state { "running" => "▶", "pending" => "◌", "scheduled" => "◔", "blocked" => "⊟", "completed" => "✓", "failed" => "✗", "skipped" | "cancelled" => "⊘", "paused" => "⏸", _ => "○" } }
pub fn route_label(route: &DagRoute) -> String {
    match route {
        DagRoute::Category { category } => format!("category:{}", normalize_renderer_text(category)),
        DagRoute::Agent { agent, model } => {
            let agent = normalize_renderer_text(agent);
            model.as_ref().map_or_else(|| format!("agent:{agent}"), |model| format!("agent:{agent}({})", normalize_renderer_text(model)))
        }
    }
}
pub struct DagRunRowsOptions { pub max_width: Option<usize>, pub now: Option<i64>, pub is_process_alive: Option<Arc<dyn Fn(i64)->bool + Send + Sync>> }
pub fn run_rows(run: &DagRunSnapshot, activity: Option<&BTreeMap<String, String>>) -> Vec<String> { run_rows_with_options(run, activity, None) }
pub fn run_rows_with_options(run: &DagRunSnapshot, activity: Option<&BTreeMap<String, String>>, options: Option<&DagRunRowsOptions>) -> Vec<String> {
    let total = run.waves.len();
    let current = run.waves.iter().position(|wave| wave.node_ids.iter().any(|id| run.nodes.iter().find(|node| &node.id == id).is_none_or(|node| !node.state.is_terminal()))).map_or(total, |index| index + 1);
    let completed = run.nodes.iter().filter(|node| node.state.as_str() == "completed").count();
    let running = run.nodes.iter().filter(|node| node.state.as_str() == "running").count();
    let failed = run.nodes.iter().filter(|node| node.state.as_str() == "failed").count();
    let mut counts = vec![format!("{completed}/{} done", run.nodes.len())];
    if running > 0 { counts.push(format!("{running} running")); }
    if failed > 0 { counts.push(format!("{failed} failed")); }
    let paused = run.status.as_str() == "paused";
    let is_alive=|pid| options.and_then(|options|options.is_process_alive.as_ref()).map_or_else(||default_is_process_alive(pid),|probe|probe(pid));
    let status = if paused {
        if run.lease_holder_pid.is_some_and(is_alive) { "resuming".to_string() }
        else { let active=run.nodes.iter().filter(|node| node.state == DagNodeState::Running).count(); if active > 0 { format!("suspended · {active} active") } else { "paused".into() } }
    } else { run.status.as_str().into() };
    let mut rows = vec![format!("{} {} {} wave {current}/{total} {}", if paused { "·" } else { icon(run.status.as_str()) }, excerpt_renderer_text(&run.name, Some(32)), status, counts.join(", "))];
    let mut ordered: Vec<_> = run.nodes.iter().collect();
    ordered.sort_by(|a,b| rank(a).cmp(&rank(b)).then_with(|| if rank(a)==0 { started_at_ms(a).cmp(&started_at_ms(b)) } else { recency_ms(b).cmp(&recency_ms(a)) }));
    let running=ordered.iter().filter(|node| rank(node)==0).count();
    let shown=ordered.iter().take(MAX_NODE_ROWS.max(running)).copied().collect::<Vec<_>>();
    let width=bounded_row_width(options.and_then(|options|options.max_width));
    for node in &shown { rows.push(node_row(node,activity,width,options.and_then(|options|options.now).unwrap_or_else(|| chrono::Utc::now().timestamp_millis()))); }
    if run.nodes.len() > shown.len() { rows.push(format!("  +{} more", run.nodes.len() - shown.len())); }
    rows
}

fn rank(node:&DagNode)->u8 { match node.state { DagNodeState::Running|DagNodeState::Paused=>0, DagNodeState::Pending|DagNodeState::Scheduled|DagNodeState::Blocked=>1, DagNodeState::Failed=>2, DagNodeState::Completed=>3, DagNodeState::Skipped|DagNodeState::Cancelled=>4 } }
fn parsed_timestamp(value:Option<&str>)->Option<i64> { value.and_then(|value|chrono::DateTime::parse_from_rfc3339(value).ok()).map(|date|date.timestamp_millis()) }
fn started_at_ms(node:&DagNode)->i64 { parsed_timestamp(node.started_at.as_deref()).unwrap_or(i64::MAX) }
fn recency_ms(node:&DagNode)->i64 { parsed_timestamp(node.completed_at.as_deref().or(node.started_at.as_deref())).unwrap_or(0) }
fn bounded_row_width(width:Option<usize>)->usize { width.filter(|width|*width>0).map_or(NODE_ROW_WIDTH_MAX,|width|width.min(NODE_ROW_WIDTH_MAX)) }
fn duration(ms:i64)->String { let seconds=ms.max(0)/1000; let hours=seconds/3600; let minutes=(seconds%3600)/60; let seconds=seconds%60; if hours>0 {format!("{hours}h {minutes}m {seconds}s")} else if minutes>0 {format!("{minutes}m {seconds}s")} else {format!("{seconds}s")} }
fn elapsed(node:&DagNode,now:i64)->Option<String> {
    let started=parsed_timestamp(node.started_at.as_deref());
    if let Some(started)=started { let end=if TERMINAL.contains(&node.state) {parsed_timestamp(node.completed_at.as_deref()).unwrap_or(now)} else {now}; Some(duration(end.saturating_sub(started))) }
    else { waiting_label(node,now) }
}
fn waiting_label(node:&DagNode,now:i64)->Option<String> { if TERMINAL.contains(&node.state) {return None;} parsed_timestamp(Some(&node.created_at)).map(|created|format!("waiting {}",duration(now.saturating_sub(created)))) }
fn node_row(node:&DagNode,activity:Option<&BTreeMap<String,String>>,width:usize,now:i64)->String {
    let head=format!("  {} ",icon(node.state.as_str())); let label=normalize_renderer_text(node.label.as_deref().unwrap_or(&node.id)); let route=route_label(&node.route);
    let attempt=(node.attempt>1).then(||format!("x{}",node.attempt));
    let live=(!TERMINAL.contains(&node.state)).then(||activity.and_then(|items|items.get(&node.id))).flatten().map(|text|normalize_renderer_text(text));
    let show_activity=live.is_some() && width>=NARROW_ROW_WIDTH; let elapsed=elapsed(node,now);
    let excerpt=|text:&str,cap:usize|excerpt_renderer_text(text,Some(cap));
    let mut parts=vec![excerpt(&label,LABEL_MIN),excerpt(&route,ROUTE_MIN)];
    if let Some(value)=&attempt {parts.push(value.clone());}
    if show_activity {parts.push(excerpt(live.as_deref().unwrap_or_default(),ACTIVITY_MIN));}
    if let Some(value)=&elapsed {parts.push(value.clone());}
    let mut remaining=width.saturating_sub(renderer_visible_width(&format!("{head}{}",parts.join(SEPARATOR))));
    let activity_width=if show_activity {let value=renderer_visible_width(live.as_deref().unwrap_or_default()).min(ACTIVITY_MIN+remaining);remaining=remaining.saturating_sub(value.saturating_sub(ACTIVITY_MIN));value} else {0};
    let route_width=renderer_visible_width(&route).min(ROUTE_MIN+remaining); remaining=remaining.saturating_sub(route_width.saturating_sub(ROUTE_MIN)); let label_width=LABEL_MAX.min(LABEL_MIN+remaining);
    let mut parts=vec![excerpt(&label,label_width),excerpt(&route,route_width)];
    if let Some(value)=attempt {parts.push(value);}
    if show_activity {parts.push(excerpt(live.as_deref().unwrap_or_default(),activity_width));}
    if let Some(value)=elapsed {parts.push(value);}
    format!("{head}{}",excerpt(&parts.join(SEPARATOR),width.saturating_sub(renderer_visible_width(&head))))
}
