//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/working-task.ts`.

use super::types::SharedSections;

pub const DEFAULT_WORKING_TASK_OPENING: &str = "Independent calls (reads, searches, listings, diagnostics) -> one parallel wave; thin context -> go wide now, pull anything loosely relevant, not serially later. Edits + result-dependent calls -> one at a time, each compared with intended state; result must be seen -> render after each change, look before next. Never fill missing parameters with placeholders.";

const FULL_WORKING_TASK_OPENING: &str = "Fire independent tool calls as one parallel wave - reads, searches, listings, diagnostics - and bias toward breadth when context is thin: pull in anything even loosely relevant now instead of serially later. Wasted reads cost almost nothing; stale assumptions cost the turn. Edits and result-dependent calls go one at a time, each compared with the state you meant to produce; when the result must be seen rather than read, render after each change and look before the next. Never fill missing parameters with placeholders.";

/// `buildWorkingTaskSection`.
pub fn build_working_task_section() -> String {
    build_working_task_section_with_options(None, SharedSections::Diet)
}

pub fn build_working_task_section_with_options(
    opening: Option<&str>,
    shared_sections: SharedSections,
) -> String {
    match shared_sections {
        SharedSections::Full => format!(
            "## Working the Task

{}

Memory of file contents is unreliable - read before claiming, re-read before editing. Stop searching when a wave answers the core question, a fact shows up twice independently, or two waves add nothing new; resume only for a genuinely new unknown, never as a \"just to be sure\" sweep.

Make one reasonable plan and execute it; reopen it only when new evidence contradicts it. Do not re-derive facts already established in the conversation or re-litigate decisions the user has made. When weighing a choice, give a recommendation, not a survey. When a delegation tool is available, hand sizeable independent tracks to subagents and keep working while they run; keep work you can finish in a few calls yourself.",
            opening.unwrap_or(FULL_WORKING_TASK_OPENING)
        ),
        SharedSections::Diet => format!(
            "## Working the Task

{}

Memory of file contents unreliable: read before claiming, re-read before editing. Stop searching when a wave answers the core question, a fact appears twice independently, or two waves add nothing; resume only for a genuinely new unknown, never as a \"just to be sure\" sweep.

One reasonable plan, execute; reopen only when new evidence contradicts it. Don't re-derive established facts or re-litigate user decisions. Weighing a choice -> recommendation, not survey. Delegation tool available: hand sizeable independent tracks to subagents, keep working meanwhile; few-call work -> yourself.",
            opening.unwrap_or(DEFAULT_WORKING_TASK_OPENING)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_working_task_section_keeps_its_heading_and_closing_line() {
        let section = build_working_task_section();
        assert!(section.starts_with("## Working the Task\n\nIndependent calls"));
        assert!(section.ends_with("few-call work -> yourself."));
        let full = build_working_task_section_with_options(None, SharedSections::Full);
        assert!(full.ends_with("keep work you can finish in a few calls yourself."));
    }
}
