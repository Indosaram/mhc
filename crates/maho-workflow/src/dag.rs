//! The `/dag` command text (spec §5).
use crate::scheduler::WorkflowScheduler;
use crate::state::NodeState;
use crate::tool::Caller;
use crate::now_ms;
use std::collections::BTreeSet;
use std::sync::Arc;

/// Most runs `/dag` lists.
pub const LIST_LIMIT: usize = 20;

/// Removes control characters (terminal escape sequences included) from user-supplied text.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c == '\n' || c == '\t' { ' ' } else { c })
        .filter(|c| !c.is_control())
        .collect()
}

fn duration(ms: u64) -> String {
    let secs = ms / 1000;
    if secs >= 3600 {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{secs}.{}s", (ms % 1000) / 100)
    }
}

/// Whether the text is a warning (unknown run id) rather than information.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DagOutput {
    pub text: String,
    pub warning: bool,
}

/// `/dag` (no argument) or `/dag <run id>`.
pub fn render(scheduler: &Arc<WorkflowScheduler>, caller: &Caller, args: &str) -> DagOutput {
    let args = args.trim();
    if args.is_empty() {
        return DagOutput { text: list(scheduler, caller), warning: false };
    }
    match scheduler.snapshot(caller, Some(args)) {
        Ok(_) => DagOutput { text: detail(scheduler, caller, args), warning: false },
        Err(_) => DagOutput {
            text: format!("No workflow run \"{}\" in this session.", sanitize(args)),
            warning: true,
        },
    }
}

fn list(scheduler: &Arc<WorkflowScheduler>, caller: &Caller) -> String {
    let runs = scheduler.list(&caller.session_id);
    if runs.is_empty() {
        return "No workflow runs in this session.".to_owned();
    }
    runs.iter()
        .take(LIST_LIMIT)
        .map(|run| {
            let (done, total, running, failed) = run.counts();
            let mut line = format!(
                "{}  {}  {}  {done}/{total} done",
                sanitize(&run.name),
                run.run_id,
                run.status.as_str()
            );
            if running > 0 {
                line.push_str(&format!(", {running} running"));
            }
            if failed > 0 {
                line.push_str(&format!(", {failed} failed"));
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn detail(scheduler: &Arc<WorkflowScheduler>, caller: &Caller, run_id: &str) -> String {
    let Ok(snapshot) = scheduler.snapshot(caller, Some(run_id)) else {
        return String::new();
    };
    let mut lines = Vec::new();
    let mut header = format!(
        "{} ({}) {} - {} nodes in {} waves",
        sanitize(&snapshot.name),
        snapshot.run_id,
        snapshot.status.as_str(),
        snapshot.total,
        snapshot.graph.waves.len()
    );
    if snapshot.amendments > 0 {
        header.push_str(&format!(", {} amendment(s)", snapshot.amendments));
    }
    lines.push(header);
    let critical: BTreeSet<&str> = snapshot.graph.critical_path.iter().map(String::as_str).collect();
    let now = now_ms();
    for (index, wave) in snapshot.graph.waves.iter().enumerate() {
        lines.push(format!("Wave {}:", index + 1));
        for id in wave {
            let Some(node) = snapshot.nodes.iter().find(|n| &n.id == id) else { continue };
            let mut line = format!(
                "  {} {} [{}] {}",
                if critical.contains(id.as_str()) { "*" } else { "-" },
                sanitize(&node.label),
                node.state.as_str(),
                sanitize(&node.target)
            );
            if let Some(model) = &node.model {
                line.push_str(&format!(" model={}", sanitize(model)));
            }
            if node.attempt > 1 {
                line.push_str(&format!(" attempt={}", node.attempt));
            }
            if node.state != NodeState::Pending
                && let Some(ms) = node.duration_ms.or_else(|| node.started_at.map(|s| now.saturating_sub(s)))
            {
                line.push_str(&format!(" {}", duration(ms)));
            }
            if !node.depends_on.is_empty() {
                let deps: Vec<String> = node.depends_on.iter().map(|d| sanitize(d.as_str())).collect();
                line.push_str(&format!(" after {}", deps.join(", ")));
            }
            if let Some(error) = &node.error {
                line.push_str(&format!(" error {}: {}", error.code, sanitize(&error.message)));
            }
            lines.push(line);
        }
    }
    let path: Vec<String> = snapshot.graph.critical_path.iter().map(|id| sanitize(id.as_str())).collect();
    lines.push(format!("Critical path (*): {}", path.join(" -> ")));
    let bottlenecks: Vec<String> = snapshot
        .graph
        .bottlenecks
        .iter()
        .filter(|b| b.dependents > 0)
        .map(|b| format!("{} ({})", sanitize(&b.id), b.dependents))
        .collect();
    lines.push(format!(
        "Bottlenecks: {}",
        if bottlenecks.is_empty() { "none".to_owned() } else { bottlenecks.join(", ") }
    ));
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_control_characters() {
        assert_eq!(sanitize("a\u{1b}[31mb\u{7}c\nd"), "a[31mbc d");
    }
}
