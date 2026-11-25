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

    use crate::presets::{ModelMetadata, resolve_preset};
    use crate::settings::PromptPresetName;
    use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;

    fn render(preset: PromptPresetName, surface: PromptSurface) -> String {
        let options = BuildDynamicSystemPromptOptions {
            append_system_prompt: None,
            custom_prompt: None,
            on_parts: None,
            cwd: "/work".into(),
            selected_tools: vec![],
            tool_snippets: Default::default(),
            prompt_guidelines: vec![],
            full_tool_prompt: None,
            shared_sections: Default::default(),
            native_search_available: false,
            context_files: vec![],
            skills: vec![],
            tuning_section: None,
            core_prompt: None,
            workstation_dialect: None,
            surface,
        };
        resolve_preset(
            ModelMetadata {
                id: "unmatched",
                provider: "fixture",
                name: None,
                prompt_preset: None,
            },
            preset,
            options,
        )
        .expect("preset renders")
        .prompt
    }

    /// Each GPT core renders the moments for its own surface, and chat swaps the block out.
    #[test]
    fn the_gpt_cores_render_the_moments_for_their_surface() {
        for preset in [
            PromptPresetName::Gpt55,
            PromptPresetName::Gpt56,
            PromptPresetName::Gpt6Astra,
        ] {
            for surface in [PromptSurface::Terminal, PromptSurface::App] {
                let prompt = render(preset, surface);
                assert!(
                    prompt.contains(gpt_handoff_moments(surface)),
                    "{} on {surface:?} carries its own moments",
                    preset.as_str()
                );
                let other = if surface == PromptSurface::App {
                    PromptSurface::Terminal
                } else {
                    PromptSurface::App
                };
                assert!(
                    !prompt.contains(gpt_handoff_moments(other)),
                    "{} on {surface:?} does not carry the {other:?} moments",
                    preset.as_str()
                );
            }
            let chat = render(preset, PromptSurface::Chat);
            assert!(
                !chat.contains("## Handoff"),
                "{} chat has no block",
                preset.as_str()
            );
            assert!(
                chat.contains(maho_core::dynamic_prompt::handoff::CHAT_REPLY_RULE),
                "{} chat renders the reply rule",
                preset.as_str()
            );
        }
    }

    #[test]
    fn chat_folds_into_the_terminal_wording() {
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
