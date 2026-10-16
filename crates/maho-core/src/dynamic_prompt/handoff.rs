use super::PromptSurface;

const HANDOFF_LANGUAGE_RULE: &str = "Keep the labels Ask, wanted, For you, Now, and Next exactly as written; write everything else - the routing line, slot contents, todo labels, and the reply itself - in the user's language (the one their instructions name, else the one they write in), with Now and Next repeating the todo labels verbatim.";
const APP_HANDOFF_LANGUAGE_RULE: &str = "Keep the labels Ask, wanted, For you, Now, and Next exactly as written; write everything else - slot contents, todo labels, and the reply itself - in the user's language (the one their instructions name, else the one they write in), with Now and Next repeating the todo labels verbatim.";
const CHAT_REPLY_RULE: &str = "Each reply is posted as a chat message to the people in a conversation. Write the answer itself in their language, with no status block, todo labels, or progress ledger; the plan and its progress live in the todo tool only.";

pub const CHAT_FINAL_MESSAGE: &str = "The final message is the answer itself";

pub fn build_handoff_section(surface: PromptSurface, turn_end_rule_stated_elsewhere: bool) -> String {
    if surface == PromptSurface::Chat {
        return format!("## Replies\n\n{CHAT_REPLY_RULE}");
    }
    let (moment, language) = match surface {
        PromptSurface::Terminal => ("A handoff is a multi-task todo list's creation (in the message that creates it, after the routing line, or the next one), each todo phase change, a blocker or plan change, and the end of a turn that changed something; the routing line is not one, and an answer stays an answer, even after tool calls.", HANDOFF_LANGUAGE_RULE),
        PromptSurface::App => ("A handoff is a multi-task todo list's creation (in the message that creates it or the next one), each todo phase change, a blocker or plan change, and the end of a turn that changed something; an answer stays an answer, even after tool calls.", APP_HANDOFF_LANGUAGE_RULE),
        PromptSurface::Chat => return format!("## Replies\n\n{CHAT_REPLY_RULE}"),
    };
    let next = if turn_end_rule_stated_elsewhere { "The Next you name is executed in this same response with tool calls." } else { "The Next you name is executed in this same response with tool calls; a Next with nothing after it is a defect." };
    let between = if surface == PromptSurface::Terminal { "Between handoffs, work without narration." } else { "Between handoffs, after each tool wave that changes what you know, write one line of reply text: what you found, then `Now: [task]. Next: [task].`" };
    format!("## Handoff\n\n{moment} Before writing one, weigh what the user originally asked for and what they would want to know right now; then state it in one short block:\n\nAsk: [the user's original request] - wanted: [the outcome they asked for]. For you: [what they need to know now - ledger N/M done, findings, blockers]. Now: [the todo task in progress]. Next: [the next open task].\n\n{language} {next} {between}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_rendering_respects_each_prompt_surface() {
        let terminal = build_handoff_section(PromptSurface::Terminal, true);
        let app = build_handoff_section(PromptSurface::App, true);
        let chat = build_handoff_section(PromptSurface::Chat, true);
        assert!(terminal.contains("after the routing line"));
        assert!(app.contains("Between handoffs"));
        assert!(!app.contains("routing line"));
        assert!(chat.starts_with("## Replies"));
        assert!(!chat.contains("Ask:"));
    }
}
