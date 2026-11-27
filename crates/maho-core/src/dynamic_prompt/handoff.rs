//! Port of `dynamic-prompt/handoff.ts`.
use super::PromptSurface;
use super::types::SharedSections;

pub const HANDOFF_LANGUAGE_RULE: &str = "Labels Ask, wanted, For you, Now, Next: exactly as written. Everything else (routing line, slot contents, todo labels, reply) in user's language (named in their instructions, else the one they write in); Now/Next repeat todo labels verbatim.";

pub const FULL_HANDOFF_LANGUAGE_RULE: &str = "Keep the labels Ask, wanted, For you, Now, and Next exactly as written; write everything else - the routing line, slot contents, todo labels, and the reply itself - in the user's language (the one their instructions name, else the one they write in), with Now and Next repeating the todo labels verbatim.";

/// The app surface has no routing line, so its language rule drops that clause; kept as the same
/// derivation upstream uses so the two rules cannot drift apart.
fn app_handoff_language_rule() -> String {
    HANDOFF_LANGUAGE_RULE.replace("routing line, ", "")
}

fn full_app_handoff_language_rule() -> String {
    FULL_HANDOFF_LANGUAGE_RULE.replace("the routing line, ", "")
}

const HANDOFF_MOMENTS_TERMINAL: &str = "Handoff moments: multi-task todo list created (same message after routing line, or next one), each todo phase change, blocker or plan change, end of a turn that changed something. Routing line is not one; an answer stays an answer even after tool calls.";
const HANDOFF_MOMENTS_APP: &str = "Handoff moments: multi-task todo list created (same message or next one), each todo phase change, blocker or plan change, end of a turn that changed something. An answer stays an answer even after tool calls.";

const FULL_HANDOFF_MOMENTS_TERMINAL: &str = "A handoff is a multi-task todo list's creation (in the message that creates it, after the routing line, or the next one), each todo phase change, a blocker or plan change, and the end of a turn that changed something; the routing line is not one, and an answer stays an answer, even after tool calls.";
const FULL_HANDOFF_MOMENTS_APP: &str = "A handoff is a multi-task todo list's creation (in the message that creates it or the next one), each todo phase change, a blocker or plan change, and the end of a turn that changed something; an answer stays an answer, even after tool calls.";

pub const CHAT_REPLY_RULE: &str = "Each reply is posted as a chat message to the people in the conversation. Write the answer itself in their language, with no status block, todo labels, or progress ledger; the plan and its progress live in the todo tool only.";

/// `## Replies` section rendered in place of the handoff block on `chat`.
pub fn chat_replies_section() -> String {
    format!("## Replies\n\n{CHAT_REPLY_RULE}")
}

pub const CHAT_FINAL_MESSAGE: &str = "The final message is the answer itself";

/// `HandoffSectionOptions`.
#[derive(Debug, Clone, Copy, Default)]
pub struct HandoffSectionOptions {
    /// The caller's core already states a text-only turn-end rule ("check your last paragraph"),
    /// so the block drops its own "a Next with nothing after it is a defect" clause instead of
    /// stating that rule twice.
    pub turn_end_rule_stated_elsewhere: bool,
    /// The model under-reports during long tool chains by default (Claude Fable 5.1 guide, "Ask for
    /// user-facing progress updates": remove narration-suppressing lines, then say when user-facing
    /// text is wanted and what each update contains), so the quiet-between-handoffs sentence becomes
    /// a brief-update instruction naming the moment and the shape. Rendered only by the Claude Fable
    /// 5.1 preset (senpi#2681).
    pub brief_updates_between_handoffs: bool,
    /// The app surface has no routing line, so the block stops referring to one; chat has no block at all.
    pub surface: PromptSurface,
}

pub fn build_handoff_section(options: HandoffSectionOptions) -> String {
    build_handoff_section_for_sections(options, SharedSections::Diet)
}

pub fn build_handoff_section_for_sections(
    options: HandoffSectionOptions,
    shared_sections: SharedSections,
) -> String {
    let surface = options.surface;
    if surface == PromptSurface::Chat {
        return chat_replies_section();
    }
    if shared_sections == SharedSections::Full {
        return build_full_handoff_section(options);
    }
    let next_rule = if options.turn_end_rule_stated_elsewhere {
        "The named Next is executed in this same response with tool calls."
    } else {
        "The named Next is executed in this same response with tool calls; a Next with nothing after it is a defect."
    };
    let between_rule = if options.brief_updates_between_handoffs {
        "Between handoffs: after each tool wave that changes what you know, one line of reply text - what you found, then `Now: [task]. Next: [task].`"
    } else {
        "Between handoffs: work without narration."
    };
    let (moment, language_rule) = if surface == PromptSurface::App {
        (HANDOFF_MOMENTS_APP, app_handoff_language_rule())
    } else {
        (HANDOFF_MOMENTS_TERMINAL, HANDOFF_LANGUAGE_RULE.to_owned())
    };
    format!(
        "## Handoff\n\n{moment} First weigh original ask + what user needs now; then one short block:\n\nAsk: [the user's original request] - wanted: [the outcome they asked for]. For you: [what they need to know now - ledger N/M done, findings, blockers]. Now: [the todo task in progress]. Next: [the next open task].\n\n{language_rule} {next_rule} {between_rule}"
    )
}

fn build_full_handoff_section(options: HandoffSectionOptions) -> String {
    let surface = options.surface;
    let next_rule = if options.turn_end_rule_stated_elsewhere {
        "The Next you name is executed in this same response with tool calls."
    } else {
        "The Next you name is executed in this same response with tool calls; a Next with nothing after it is a defect."
    };
    let between_rule = if options.brief_updates_between_handoffs {
        "Between handoffs, after each tool wave that changes what you know, write one line of reply text: what you found, then `Now: [task]. Next: [task].`"
    } else {
        "Between handoffs, work without narration."
    };
    let (moment, language_rule) = if surface == PromptSurface::App {
        (FULL_HANDOFF_MOMENTS_APP, full_app_handoff_language_rule())
    } else {
        (
            FULL_HANDOFF_MOMENTS_TERMINAL,
            FULL_HANDOFF_LANGUAGE_RULE.to_owned(),
        )
    };
    format!(
        "## Handoff\n\n{moment} Before writing one, weigh what the user originally asked for and what they would want to know right now; then state it in one short block:\n\nAsk: [the user's original request] - wanted: [the outcome they asked for]. For you: [what they need to know now - ledger N/M done, findings, blockers]. Now: [the todo task in progress]. Next: [the next open task].\n\n{language_rule} {next_rule} {between_rule}"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_rendering_respects_each_prompt_surface() {
        let terminal = build_handoff_section(HandoffSectionOptions {
            turn_end_rule_stated_elsewhere: true,
            ..Default::default()
        });
        let app = build_handoff_section(HandoffSectionOptions {
            turn_end_rule_stated_elsewhere: true,
            surface: PromptSurface::App,
            ..Default::default()
        });
        let chat = build_handoff_section(HandoffSectionOptions {
            surface: PromptSurface::Chat,
            ..Default::default()
        });
        assert!(terminal.contains("after routing line"));
        let full = build_handoff_section_for_sections(
            HandoffSectionOptions {
                turn_end_rule_stated_elsewhere: true,
                ..Default::default()
            },
            SharedSections::Full,
        );
        assert!(full.contains("after the routing line"));
        assert!(app.contains("Between handoffs"));
        assert!(!app.contains("routing line"));
        assert!(chat.starts_with("## Replies"));
        assert!(!chat.contains("Ask:"));
    }

    /// senpi#2681: only the preset that opts in gets the brief-update sentence, and it replaces
    /// the quiet-between-handoffs rule rather than joining it.
    #[test]
    fn brief_updates_between_handoffs_is_opt_in() {
        let quiet = build_handoff_section(HandoffSectionOptions::default());
        let brief = build_handoff_section(HandoffSectionOptions {
            brief_updates_between_handoffs: true,
            ..Default::default()
        });
        assert_ne!(quiet, brief);
        assert_eq!(brief.matches("Between handoffs").count(), 1);
        assert_eq!(quiet.matches("Between handoffs").count(), 1);
        assert!(!quiet.contains("Now: [task]. Next: [task]."));
        assert!(brief.contains("`Now: [task]. Next: [task].`"));
    }
}
