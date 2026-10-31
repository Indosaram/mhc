//! Port of `prompt-preset/generation.ts` (senpi 903b8336): the stable entry points other crates use to
//! render a preset without reaching into the per-model modules.

pub use maho_core::dynamic_prompt::build::{
    BuildDynamicSystemPromptOptions, build_dynamic_system_prompt,
};

use crate::{
    presets::{ModelMetadata, ResolvedPromptPreset, resolve_preset as native_resolve},
    settings::PromptPresetName,
};

/// `buildPreset(name, options)`: renders the named preset. `None` only for `Auto`, which upstream's
/// `ResolvedPresetName` excludes and which has no prompt of its own.
pub fn build_preset(
    name: PromptPresetName,
    options: BuildDynamicSystemPromptOptions<'_>,
) -> Option<ResolvedPromptPreset> {
    if name == PromptPresetName::Auto {
        return None;
    }
    // A non-auto setting resolves to itself without reading the model.
    let model = ModelMetadata {
        id: "",
        provider: "",
        name: None,
        prompt_preset: None,
    };
    native_resolve(model, name, options)
}

/// `resolvePreset(model, settings, options)`.
pub fn resolve_preset(
    model: ModelMetadata<'_>,
    setting: PromptPresetName,
    options: BuildDynamicSystemPromptOptions<'_>,
) -> Option<ResolvedPromptPreset> {
    native_resolve(model, setting, options)
}

#[cfg(test)]
mod tests {
    //! Port of `test/suite/prompt-presets-generation.test.ts` cases that do not need the
    //! committed native hash baseline.
    use super::*;
    use maho_core::{dynamic_prompt::PromptSurface, system_prompt::ContextFile};

    fn options() -> BuildDynamicSystemPromptOptions<'static> {
        BuildDynamicSystemPromptOptions {
            custom_prompt: None,
            append_system_prompt: None,
            on_parts: None,
            cwd: "/fixture".into(),
            selected_tools: ["read", "eval", "apply_patch", "todo", "ask_user_question"]
                .map(String::from)
                .to_vec(),
            tool_snippets: std::collections::HashMap::from([
                ("read".to_owned(), "READ_SENTINEL".to_owned()),
                ("eval".to_owned(), "EVAL_SENTINEL".to_owned()),
            ]),
            prompt_guidelines: vec!["GUIDELINE_SENTINEL".into()],
            context_files: vec![ContextFile {
                path: "/fixture/AGENTS.md".into(),
                content: "CONTEXT_SENTINEL".into(),
            }],
            skills: Vec::new(),
            tuning_section: None,
            core_prompt: None,
            workstation_dialect: None,
            surface: PromptSurface::Terminal,
        }
    }

    fn model<'a>(id: &'a str, provider: &'a str, preset: Option<&'a str>) -> ModelMetadata<'a> {
        ModelMetadata {
            id,
            provider,
            name: None,
            prompt_preset: preset,
        }
    }

    #[test]
    fn build_preset_renders_the_named_preset_and_none_for_auto() {
        assert!(build_preset(PromptPresetName::Auto, options()).is_none());
        let built = build_preset(PromptPresetName::Gpt54, options()).expect("gpt-5.4 preset");
        assert_eq!(built.name, PromptPresetName::Gpt54);
        assert_eq!(built.prompt, crate::gpt_5_4::build_gpt54_prompt(options()));
        assert!(built.prompt.contains("CONTEXT_SENTINEL"));
    }

    #[test]
    fn forced_presets_resolve_to_themselves_for_an_unknown_model() {
        for name in [
            PromptPresetName::Gpt5,
            PromptPresetName::Gpt52,
            PromptPresetName::ClaudeOpus55,
            PromptPresetName::Grok45,
        ] {
            let generated = resolve_preset(model("unknown", "synthetic", None), name, options())
                .expect("forced preset");
            assert_eq!(generated.name, name);
            let built = build_preset(name, options()).expect("built preset");
            assert_eq!(generated.prompt, built.prompt, "{}", name.as_str());
        }
    }

    #[test]
    fn preserves_explicit_settings_and_metadata_precedence() {
        let model = model("gpt-5.4", "openai", Some("claude-opus-5-5"));
        let automatic =
            resolve_preset(model, PromptPresetName::Auto, options()).expect("automatic");
        let forced = resolve_preset(model, PromptPresetName::Gpt52, options()).expect("forced");
        assert_eq!(automatic.name, PromptPresetName::ClaudeOpus55);
        assert_eq!(forced.name, PromptPresetName::Gpt52);
        assert_eq!(
            automatic.prompt,
            crate::claude_opus_5_5::build_claude_opus55_prompt(options())
        );
        assert_eq!(forced.prompt, crate::gpt_5_2::build_gpt52_prompt(options()));
    }

    #[test]
    fn exposes_the_default_builder_for_unknown_models() {
        let model = model("unknown-model", "synthetic", None);
        assert!(resolve_preset(model, PromptPresetName::Auto, options()).is_none());
        let default = build_dynamic_system_prompt(&options());
        assert!(default.contains("CONTEXT_SENTINEL"));
        assert_ne!(
            default,
            build_preset(PromptPresetName::Gpt54, options())
                .expect("gpt-5.4")
                .prompt
        );
    }
}
