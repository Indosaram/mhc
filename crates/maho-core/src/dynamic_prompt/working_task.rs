//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/working-task.ts`.

/// `buildWorkingTaskSection`.
pub fn build_working_task_section() -> String {
    "## Working the Task

Fire independent tool calls as one parallel wave - reads, searches, listings, diagnostics - and bias toward breadth when context is thin: pull in anything even loosely relevant now instead of serially later. Wasted reads cost almost nothing; stale assumptions cost the turn. Edits and result-dependent calls go one at a time, each compared with the state you meant to produce; when the result must be seen rather than read, render after each change and look before the next. Never fill missing parameters with placeholders.

Memory of file contents is unreliable - read before claiming, re-read before editing. Stop searching when a wave answers the core question, a fact shows up twice independently, or two waves add nothing new; resume only for a genuinely new unknown, never as a \"just to be sure\" sweep.

Make one reasonable plan and execute it; reopen it only when new evidence contradicts it. Do not re-derive facts already established in the conversation or re-litigate decisions the user has made. When weighing a choice, give a recommendation, not a survey. When a delegation tool is available, hand sizeable independent tracks to subagents and keep working while they run; keep work you can finish in a few calls yourself."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_task_section_keeps_its_heading_and_closing_line() {
        let section = build_working_task_section();
        assert!(section.starts_with("## Working the Task\n\nFire independent tool calls"));
        assert!(section.ends_with("keep work you can finish in a few calls yourself."));
    }
}
