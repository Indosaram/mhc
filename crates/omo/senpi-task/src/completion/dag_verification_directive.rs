//! `completion/dag-verification-directive.ts`: the model-facing directives appended to DAG
//! completion payloads.
//!
//! A DAG node's own summary is unverified self-report, so the orchestrating parent is told to
//! re-derive the node's scope and prove every deliverable itself before treating the node as done.
//! The Astra variants are the GPT-6 Astra parent's spot-check forms: its prior is already to verify
//! broadly, and "run the commands it claims pass" made it rerun every green suite per node.

pub const DAG_VERIFICATION_DIRECTIVE: &str = "DAG SUBAGENT COMPLETION - TREAT AS FALSE UNTIL YOU PROVE IT.\nThis completion arrived from a DAG subagent. Assume it overstated or fabricated its work. Its summary is a CLAIM, not evidence.\nBefore relying on this result you MUST, in order:\n1. RECONSTRUCT the node's full work scope from its prompt: every deliverable, file, and check it owed.\n2. READ the actual artifacts yourself - open every file it claims it changed, run the commands it claims pass. Transcripts and summaries prove NOTHING.\n3. VERIFY each deliverable against that scope with your own eyes and your own tool calls - in BOTH directions: nothing owed is missing, and nothing beyond the scope was done. Over-engineering, drive-by refactors, and edits outside the prompt's scope are defects, not bonus work.\nIf ANY deliverable is missing, partial, or unproven, or the node drifted out of scope: send precise corrective instructions to THIS node (dag action \"send\" with this run_id and node_id; \"retry\" when it cannot be continued) and demand the fix WITH evidence: complete what is missing, revert what fell outside the scope. Loop until your own verification passes.\nWork is done ONLY when you have verified it yourself.";

pub const ASTRA_DAG_VERIFICATION_DIRECTIVE: &str = "DAG SUBAGENT COMPLETION - A CLAIM UNTIL ITS EVIDENCE CHECKS OUT.\nRead the node's VERIFY output against the scope its prompt set, in both directions: every owed deliverable present, nothing done beyond the scope. Rerun a check only when that output is missing, failing, or contradicts the scope.\nIf a deliverable is missing, partial, unproven, or out of scope, send corrective instructions to THIS node (workflow action \"send\" with this run_id and node_id; \"retry\" when it cannot be continued) and require the fix with its VERIFY output.";

pub const ASTRA_DAG_RUN_VERIFICATION_DIRECTIVE: &str = "DAG RUN COMPLETION.\nThe run's verification node owns the combined checks: read its output against the run's requirements and the node summaries above. A run without one gets a single pass of the checks its requirements name, run once. Rerun a check only when its output is missing, failing, or contradicts a requirement; correct incomplete or out-of-scope work through the affected node. Report unresolved failures or blockers.";

/// Every directive a delivered wake may carry, in the order the selector scans them.
pub const DAG_VERIFICATION_DIRECTIVES: [&str; 3] = [
    DAG_VERIFICATION_DIRECTIVE,
    ASTRA_DAG_VERIFICATION_DIRECTIVE,
    ASTRA_DAG_RUN_VERIFICATION_DIRECTIVE,
];
