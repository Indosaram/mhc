//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/intent-gate.ts`.

use super::tool_categorization::get_tools_prompt_display;
use super::types::AvailableTool;

/// `buildKeyTriggers`.
pub fn build_key_triggers(tools: &[AvailableTool]) -> String {
    let trigger_tools = get_tools_prompt_display(tools);

    if trigger_tools.is_empty() {
        return String::new();
    }

    format!("\nSpecialized search available this turn: {trigger_tools}. Prefer them for locating symbols, files, and patterns; never mention a tool this turn does not have.\n")
}

/// `buildIntentGate`.
pub fn build_intent_gate(tools: &[AvailableTool]) -> String {
    format!(
        "## Intent Gate

Open every turn with one short routing line:

> I read this as [intent] - [plan]. I'll stop when [the observable condition that ends this turn].

The line keeps your reading transparent; only the user's explicit request commits you to implementation. Name the stop condition as an end state you can observe, not a step count; once it holds, deliver the final message and stop. Never surface other prompt scaffolding (\"Step 0\", \"Thinking level\", XML tool-call examples) in user-facing output.
{}
Route by true intent, not surface form:
- Information asks (explain, look into, investigate): read the code, report the answer or findings - no edits, no fixes yet.
- Judgment asks (what do you think, review) and open-ended changes (refactor, improve, clean up): assess and propose, then wait for confirmation.
- Change asks (implement, add, fix this error): build, or diagnose and fix minimally, at exactly the asked scope - the smallest path that fully satisfies an open-ended goal; name an ambiguity and resolve it from context when possible.

Deliver the task at the scope asked - never quietly narrow, widen, or swap it. Make routine judgment calls yourself; ask only when different readings of the request would lead to materially different work.

Derive intent from the latest user turn alone: a new direction drops the stale plan; queued steering messages outrank earlier intent. Inspect the code, tests, or runtime the answer depends on; once context is sufficient, act - do not keep browsing.",
        build_key_triggers(tools)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dynamic_prompt::tool_categorization::categorize_tools;

    #[test]
    fn search_tools_are_advertised_in_the_routing_line() {
        let tools = categorize_tools(&["grep".to_string(), "glob".to_string()]);
        let gate = build_intent_gate(&tools);
        assert!(gate.contains("\nSpecialized search available this turn: `grep`, `glob`. Prefer them for locating symbols, files, and patterns; never mention a tool this turn does not have.\n"));
    }

    #[test]
    fn without_search_tools_the_trigger_sentence_is_omitted() {
        let gate = build_intent_gate(&categorize_tools(&["read".to_string()]));
        assert!(!gate.contains("Specialized search available this turn"));
        assert!(gate.contains("in user-facing output.\n\nRoute by true intent, not surface form:"));
    }
}
