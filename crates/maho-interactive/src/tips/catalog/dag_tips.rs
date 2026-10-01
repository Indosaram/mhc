//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/dag-tips.ts`.

use super::types::TipDefinition;

pub const DAG_TIPS: &[TipDefinition] = &[
    TipDefinition { id: "dag.one-keyword-mastery", bindings: &[], requires_command: Some("dag"), render: |_k| "One keyword makes you a master of graph engineering: say \"mass-ulw\" and your work becomes a dependency graph that schedules itself.".to_string() },
    TipDefinition { id: "dag.what-it-is", bindings: &[], requires_command: Some("dag"), render: |_k| "Trigger \"mass-ulw\" when some tasks must wait for others - describe the work, get a graph that runs in the right order.".to_string() },
    TipDefinition { id: "dag.parallel-waves", bindings: &[], requires_command: Some("dag"), render: |_k| "A graph run executes in waves: everything with no pending dependency starts at once, the rest starts the moment it is unblocked.".to_string() },
    TipDefinition { id: "dag.depends-on", bindings: &[], requires_command: Some("dag"), render: |_k| "Say what needs what - review after implementation, docs after both - and the ordering is enforced for you.".to_string() },
    TipDefinition { id: "dag.categories", bindings: &[], requires_command: Some("dag"), render: |_k| "Each node in the graph picks its own worker category, so cheap steps stay cheap and hard steps get the strong model.".to_string() },
    TipDefinition { id: "dag.status-view", bindings: &[], requires_command: Some("dag"), render: |_k| "Use /dag to watch a run: which nodes finished, which are running, and which one failed.".to_string() },
    TipDefinition { id: "dag.resume", bindings: &[], requires_command: Some("dag"), render: |_k| "Graph runs are journaled - if the session dies mid-run it resumes later and never redoes the nodes that already finished.".to_string() },
    TipDefinition { id: "dag.vs-parallel", bindings: &[], requires_command: Some("dag"), render: |_k| "Fully independent jobs just need parallel subagents; reach for a graph when the ordering between them is the whole point.".to_string() },
    TipDefinition { id: "dag.scale", bindings: &[], requires_command: Some("dag"), render: |_k| "Stop hand-running steps in sequence. Describe the dependencies once and let a dozen agents fan out and rejoin on their own.".to_string() },
];
