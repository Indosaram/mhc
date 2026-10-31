//! Port of `prompt-preset/public.ts` (senpi 903b8336): the preset surface other crates import.

pub use crate::contract::*;
pub use crate::generation::{
    BuildDynamicSystemPromptOptions, build_dynamic_system_prompt, build_preset, resolve_preset,
};
pub use crate::matchers::ModelWithPromptPresetMetadata;
pub use crate::presets::ResolvedPromptPreset;
pub use crate::settings::PromptPresetName;
pub use maho_core::dynamic_prompt::PromptSurface;

#[cfg(test)]
mod tests {
    //! Port of `test/suite/prompt-presets-public-builders.test.ts` ("public Native prompt
    //! generation"); the session-callback half needs the host harness and stays in tests/.
    use crate::public;
    use maho_core::system_prompt::ContextFile;

    fn options() -> public::BuildDynamicSystemPromptOptions<'static> {
        public::BuildDynamicSystemPromptOptions {
            custom_prompt: None,
            append_system_prompt: None,
            on_parts: None,
            cwd: "/synthetic-project".into(),
            selected_tools: ["read", "bash", "ask_user_question"]
                .map(String::from)
                .to_vec(),
            tool_snippets: std::collections::HashMap::from([(
                "read".to_owned(),
                "READ_CAPABILITY_SENTINEL".to_owned(),
            )]),
            prompt_guidelines: vec!["GUIDELINE_SENTINEL".into()],
            context_files: vec![ContextFile {
                path: "/synthetic-project/AGENTS.md".into(),
                content: "CONTEXT_SENTINEL".into(),
            }],
            skills: Vec::new(),
            tuning_section: None,
            core_prompt: None,
            workstation_dialect: None,
            surface: public::PromptSurface::Terminal,
        }
    }

    fn model<'a>(
        id: &'a str,
        preset: Option<&'a str>,
    ) -> public::ModelWithPromptPresetMetadata<'a> {
        public::ModelMetadata {
            id,
            provider: "synthetic",
            name: None,
            prompt_preset: preset,
        }
    }

    #[test]
    fn exports_the_runtime_resolver_not_a_second_implementation() {
        for id in ["claude-opus-5-5", "gpt-5.4", "unknown"] {
            let exported =
                public::resolve_preset(model(id, None), public::PromptPresetName::Auto, options());
            let runtime = crate::presets::resolve_preset(
                model(id, None),
                public::PromptPresetName::Auto,
                options(),
            );
            assert_eq!(
                exported.map(|preset| (preset.name, preset.prompt)),
                runtime.map(|preset| (preset.name, preset.prompt)),
                "{id}"
            );
        }
    }

    #[test]
    fn renders_each_models_actual_body() {
        type Builder = fn(public::BuildDynamicSystemPromptOptions<'_>) -> String;
        let builders: [(&str, public::PromptPresetName, Builder); 3] = [
            (
                "claude-opus-5-5",
                public::PromptPresetName::ClaudeOpus55,
                crate::claude_opus_5_5::build_claude_opus55_prompt,
            ),
            (
                "claude-fable-5-1",
                public::PromptPresetName::ClaudeFable51,
                crate::claude_fable_5_1::build_claude_fable51_prompt,
            ),
            (
                "gpt-5.4",
                public::PromptPresetName::Gpt54,
                crate::gpt_5_4::build_gpt54_prompt,
            ),
        ];
        for (id, name, build) in builders {
            assert_eq!(
                public::resolve_preset_name(model(id, None), public::PromptPresetName::Auto),
                Some(name)
            );
            let actual =
                public::resolve_preset(model(id, None), public::PromptPresetName::Auto, options())
                    .expect("preset");
            assert_eq!(actual.name, name);
            assert_eq!(actual.prompt, build(options()), "{id}");
            let built: public::ResolvedPromptPreset =
                public::build_preset(name, options()).expect("built");
            assert_eq!(built.prompt, actual.prompt, "{id}");
        }
    }

    #[test]
    fn keeps_explicit_settings_and_metadata_ahead_of_automatic_selection() {
        let model = model("claude-opus-5-5", Some("gpt-5.4"));
        let metadata = public::resolve_preset(model, public::PromptPresetName::Auto, options())
            .expect("metadata");
        let explicit = public::resolve_preset(model, public::PromptPresetName::Gpt5, options())
            .expect("explicit");
        let gpt54 =
            public::build_preset(public::PromptPresetName::Gpt54, options()).expect("gpt-5.4");
        let gpt5 = public::build_preset(public::PromptPresetName::Gpt5, options()).expect("gpt-5");
        assert_eq!((metadata.name, metadata.prompt), (gpt54.name, gpt54.prompt));
        assert_eq!((explicit.name, explicit.prompt), (gpt5.name, gpt5.prompt));
    }

    #[test]
    fn exposes_fallback_generation_without_inventing_a_preset() {
        assert!(
            public::resolve_preset(
                model("unknown", None),
                public::PromptPresetName::Auto,
                options()
            )
            .is_none()
        );
        let fallback = public::build_dynamic_system_prompt(&options());
        assert_eq!(fallback, public::build_dynamic_system_prompt(&options()));
        assert!(fallback.contains("CONTEXT_SENTINEL"));
    }
}
