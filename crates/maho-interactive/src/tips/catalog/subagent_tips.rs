//! Port of senpi `packages/coding-agent/src/modes/interactive/tips/catalog/subagent-tips.ts`.

use super::types::TipDefinition;

pub const SUBAGENT_TIPS: &[TipDefinition] = &[
    TipDefinition { id: "workflow-skills.plan", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"ulw plan\" to get an explored, decision-complete plan under .omo/plans/.".to_string() },
    TipDefinition { id: "workflow-skills.start-work", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"$start-work <plan-name>\" to execute a plan end-to-end in a fresh session.".to_string() },
    TipDefinition { id: "workflow-skills.ultrawork", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"ulw\" or \"ulw loop\" for a long, evidence-driven autonomous run in ultrawork mode.".to_string() },
    TipDefinition { id: "workflow-skills.research", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"ulw-research\" for a saturating, citation-backed investigation across the codebase, docs, and the web.".to_string() },
    TipDefinition { id: "workflow-skills.hyperplan", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"hyperplan\" to have adversarial reviewers attack a plan before you commit to it.".to_string() },
    TipDefinition { id: "workflow-skills.review", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"review work\" to run parallel goal, quality, security, and hands-on QA reviews.".to_string() },
    TipDefinition { id: "workflow-skills.init-deep", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"/init-deep\" to map a project and generate a hierarchical AGENTS.md knowledge base.".to_string() },
    TipDefinition { id: "workflow-skills.debugging", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"debug this\" for parallel hypotheses, a failing regression test, a minimal fix, and real-surface QA.".to_string() },
    TipDefinition { id: "workflow-skills.refactor", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"refactor\" for codebase-aware cleanup that pins behavior before changing structure.".to_string() },
    TipDefinition { id: "workflow-skills.remove-ai-slops", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"remove AI slop\" to lock behavior first, then strip generated-code smells without drive-by rewrites.".to_string() },
    TipDefinition { id: "workflow-skills.visual-qa", bindings: &[], requires_command: Some("tasks"), render: |_k| "Trigger \"visual QA\" to capture browser or xterm evidence and review web or terminal interfaces.".to_string() },
    TipDefinition { id: "workflow-skills.report-bug", bindings: &[], requires_command: Some("tasks"), render: |_k| "Hit a bug? Say \"report a bug\" - the report-bug skill finds the session, records the exact provider and model, routes it to the right repository, and files an evidence-backed issue only after you confirm.".to_string() },
    TipDefinition { id: "subagent-categories", bindings: &[], requires_command: Some("tasks"), render: |_k| "Delegate by category - quick, deep, ultrabrain, architect, artistry, git, writing - each runs on its own model.".to_string() },
    TipDefinition { id: "subagent-commands", bindings: &[], requires_command: Some("tasks"), render: |_k| "Use /tasks to see this session's background subagents, and /task-kill to stop one.".to_string() },
    TipDefinition { id: "subagent-config", bindings: &[], requires_command: Some("tasks"), render: |_k| "~/.omo/omo.jsonc maps every subagent category to its model, reasoning effort, and fallback chain.".to_string() },
    TipDefinition { id: "subagent-team", bindings: &[], requires_command: Some("tasks"), render: |_k| "Ask for a team when one task needs several agents at once: members share a tasklist and report back to you.".to_string() },
];
