//! Port of `prompt-preset/gpt-surface.ts` (senpi 903b8336).

use maho_core::dynamic_prompt::PromptSurface;

/// `GPT_HANDOFF_MOMENTS[terminalOrApp(surface)]`: chat folds into the terminal wording.
pub fn gpt_handoff_moments(surface: PromptSurface) -> &'static str {
    match surface {
        PromptSurface::App => GPT_HANDOFF_MOMENTS_APP,
        PromptSurface::Terminal | PromptSurface::Chat => GPT_HANDOFF_MOMENTS_TERMINAL,
    }
}

pub const GPT_HANDOFF_MOMENTS_TERMINAL: &str = "a multi-task todo list's creation (in the message that creates it, after the routing line, or the next one), a todo phase change, a blocker or plan change, the final message of a turn that changed something (an answer stays an answer, even after tool calls); the routing line is not one";
pub const GPT_HANDOFF_MOMENTS_APP: &str = "a multi-task todo list's creation (in the message that creates it or the next one), a todo phase change, a blocker or plan change, the final message of a turn that changed something (an answer stays an answer, even after tool calls)";

/// The app surface's claim audit for the GPT cores (senpi#2377), in the one place each core reports on checks.
pub const GPT_APP_UNRUN_CHECK_RULE: &str = "A check that did not run is covered by the evidence that did run; name it, with the next best check, only when no other evidence supports the claim. Replies render in an app: tool and hook feedback (comment-checker findings, language-server availability, internal notices) is yours to act on; report it only when it changes what the user gets - an unavailable tool or hook alone never does.";

/// Report slot wording on the app surface: an unrun check is listed only when nothing else covers it.
pub const GPT_APP_UNVERIFIED_SLOT: &str = "anything left unverified that no other evidence covers";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_moments_match_the_strings_the_gpt_cores_embed() {
        assert!(include_str!("gpt_5_6.rs").contains(GPT_HANDOFF_MOMENTS_TERMINAL));
        assert_eq!(
            gpt_handoff_moments(PromptSurface::Chat),
            GPT_HANDOFF_MOMENTS_TERMINAL
        );
        assert_eq!(
            gpt_handoff_moments(PromptSurface::App),
            GPT_HANDOFF_MOMENTS_APP
        );
    }
}
