//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/intent-gate.ts`.

use super::tool_categorization::get_tools_prompt_display;
use super::types::{AvailableTool, PromptSurface, SharedSections};

/// `DEFAULT_ROUTING_LINE_RULE`: the clause before "only the user's explicit request commits you";
/// presets replace it.
pub const DEFAULT_ROUTING_LINE_RULE: &str = "Line keeps your reading transparent";
const FULL_ROUTING_LINE_RULE: &str = "The line keeps your reading transparent";

/// `terminalRouting(routingLineRule)`.
fn terminal_routing(routing_line_rule: &str) -> String {
    format!(
        "Open every turn with one short routing line:\n\nI read this as [intent] - [plan]. I'll stop when [the observable condition that ends this turn].\n\n{routing_line_rule}; only user's explicit request commits you to implementation. Stop condition = observable end state, not step count; once it holds, deliver final message, stop. Never surface other prompt scaffolding to user."
    )
}

fn full_terminal_routing(routing_line_rule: &str) -> String {
    format!(
        "Open every turn with one short routing line:\n\nI read this as [intent] - [plan]. I'll stop when [the observable condition that ends this turn].\n\n{routing_line_rule}; only the user's explicit request commits you to implementation. Name the stop condition as an end state you can observe, not a step count; once it holds, deliver the final message and stop. Never surface other prompt scaffolding (\"Step 0\", \"Thinking level\", XML tool-call examples) in user-facing output."
    )
}

/// `APP_ROUTING`: no routing line on the app surface, and no "other" in the scaffolding ban.
const APP_ROUTING: &str = "Only user's explicit request commits you to implementation. Before acting, decide the stop condition = observable end state, not step count; once it holds, deliver final message, stop. Never surface prompt scaffolding to user.";
const FULL_APP_ROUTING: &str = "Only the user's explicit request commits you to implementation. Before acting, decide the stop condition - an end state you can observe, not a step count; once it holds, deliver the final message and stop. Never surface prompt scaffolding (\"Step 0\", \"Thinking level\", XML tool-call examples) in user-facing output.";

/// `buildKeyTriggers`.
pub fn build_key_triggers(tools: &[AvailableTool]) -> String {
    let trigger_tools = get_tools_prompt_display(tools);

    if trigger_tools.is_empty() {
        return String::new();
    }

    format!(
        "\nSpecialized search available this turn: {trigger_tools}. Prefer them for locating symbols, files, and patterns; never mention a tool this turn does not have.\n"
    )
}

/// `buildIntentGate({ tools, surface })`; chat takes the app wording.
pub fn build_intent_gate(tools: &[AvailableTool], surface: PromptSurface) -> String {
    build_intent_gate_with_options(tools, surface, None, SharedSections::Diet)
}

pub fn build_intent_gate_with_options(
    tools: &[AvailableTool],
    surface: PromptSurface,
    routing_line_rule: Option<&str>,
    shared_sections: SharedSections,
) -> String {
    let full = shared_sections == SharedSections::Full;
    let open = match surface {
        PromptSurface::Terminal => {
            if full {
                full_terminal_routing(routing_line_rule.unwrap_or(FULL_ROUTING_LINE_RULE))
            } else {
                terminal_routing(routing_line_rule.unwrap_or(DEFAULT_ROUTING_LINE_RULE))
            }
        }
        PromptSurface::App | PromptSurface::Chat => {
            if full {
                FULL_APP_ROUTING.to_owned()
            } else {
                APP_ROUTING.to_owned()
            }
        }
    };
    let rules = if full {
        "Route by true intent, not surface form:
- Information asks (explain, look into, investigate): read the code, report the answer or findings - no edits, no fixes yet.
- Judgment asks (what do you think, review) and open-ended changes (refactor, improve, clean up): assess and propose, then wait for confirmation.
- Change asks (implement, add, fix this error): build, or diagnose and fix at the cause, at exactly the asked scope - what fully satisfies an open-ended goal and nothing beyond it; name an ambiguity and resolve it from context when possible.

Deliver the task at the scope asked - never quietly narrow, widen, or swap it. Make routine judgment calls yourself; ask only when different readings of the request would lead to materially different work.

Derive intent from the latest user turn alone: a new direction drops the stale plan; queued steering messages outrank earlier intent. Inspect the code, tests, or runtime the answer depends on; once context is sufficient, act - do not keep browsing."
    } else {
        "Route by true intent, not surface form:
- Information ask (explain, look into, investigate): read code, report answer/findings; no edits, no fixes yet.
- Judgment ask (what do you think, review) / open-ended change (refactor, improve, clean up): assess, propose, wait for confirmation.
- Change ask (implement, add, fix this error): build, or diagnose + fix at the cause, exactly at asked scope = what fully satisfies an open-ended goal, nothing beyond; name ambiguity, resolve from context when possible.

Deliver at asked scope; never quietly narrow, widen, swap. Routine calls: decide yourself; ask only when readings diverge into materially different work.

Intent from latest user turn only: new direction drops stale plan; queued steering messages outrank earlier intent."
    };
    format!(
        "## Intent Gate\n\n{open}\n{}\n{rules}",
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
        let gate = build_intent_gate(&tools, PromptSurface::Terminal);
        assert!(gate.contains("\nSpecialized search available this turn: `grep`, `glob`. Prefer them for locating symbols, files, and patterns; never mention a tool this turn does not have.\n"));
    }

    #[test]
    fn without_search_tools_the_trigger_sentence_is_omitted() {
        let gate = build_intent_gate(
            &categorize_tools(&["read".to_string()]),
            PromptSurface::Terminal,
        );
        assert!(!gate.contains("Specialized search available this turn"));
        assert!(gate.contains("to user.\n\nRoute by true intent, not surface form:"));
    }

    #[test]
    fn includes_intent_routing_rules() {
        let result = build_intent_gate(&[], PromptSurface::Terminal);
        assert!(result.contains("## Intent Gate"));
        assert!(result.contains("Route by true intent, not surface form"));
    }

    #[test]
    fn covers_the_three_intent_families() {
        let result = build_intent_gate(&[], PromptSurface::Terminal);
        assert!(result.contains("Information ask"));
        assert!(result.contains("Judgment ask"));
        assert!(result.contains("Change ask"));
    }

    #[test]
    fn includes_scope_fidelity_rule() {
        assert!(build_intent_gate(&[], PromptSurface::Terminal).contains("asked scope"));
    }

    #[test]
    fn includes_turn_local_intent_derivation() {
        assert!(build_intent_gate(&[], PromptSurface::Terminal).contains("latest user turn"));
    }

    #[test]
    fn adds_search_trigger_when_search_tools_are_available() {
        let tools = vec![AvailableTool {
            name: "grep".to_string(),
            category: crate::dynamic_prompt::types::ToolCategory::Search,
        }];
        assert!(build_intent_gate(&tools, PromptSurface::Terminal).contains("grep"));
    }

    #[test]
    fn forces_intent_verbalization_with_a_binding_stop_condition() {
        let result = build_intent_gate(&[], PromptSurface::Terminal);
        assert!(result.contains("I read this as"));
        assert!(result.contains("I'll stop when"));
        assert!(!result.contains("Keep the routing decision internal"));
        assert!(!result.contains("Do not expose classification labels"));
    }

    #[test]
    fn routes_common_surface_forms() {
        let result = build_intent_gate(&[], PromptSurface::Terminal);
        for word in ["explain", "implement", "error", "refactor"] {
            assert!(result.contains(word), "{word}");
        }
    }

    /// `terminalOrApp(surface)`: chat takes the app wording, which has no routing line.
    #[test]
    fn the_app_and_chat_surfaces_drop_the_routing_line() {
        for surface in [PromptSurface::App, PromptSurface::Chat] {
            let gate = build_intent_gate(&[], surface);
            assert!(!gate.contains("I read this as"));
            assert!(gate.starts_with("## Intent Gate\n\nOnly user's explicit request"));
            assert!(gate.contains("Never surface prompt scaffolding"));
        }
        let terminal = build_intent_gate(&[], PromptSurface::Terminal);
        assert!(terminal.contains("I read this as"));
        assert!(terminal.contains("Never surface other prompt scaffolding"));
    }
}
