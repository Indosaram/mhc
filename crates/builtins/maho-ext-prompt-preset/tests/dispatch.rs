use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
use maho_ext_prompt_preset::{presets::{ModelMetadata, resolve_preset}, settings::PromptPresetName};
fn options() -> BuildDynamicSystemPromptOptions<'static> {
    BuildDynamicSystemPromptOptions { custom_prompt: None, append_system_prompt: None, on_parts: None, cwd: "/tmp/project".into(), selected_tools: vec!["eval".into(), "read".into()], tool_snippets: Default::default(), prompt_guidelines: Vec::new(), context_files: Vec::new(), skills: Vec::new(), tuning_section: None, core_prompt: None, workstation_dialect: None, surface: maho_core::dynamic_prompt::PromptSurface::Terminal }
}
#[test]
fn surface_is_visible_to_core_prompt_overrides() {
    let core = |context: &maho_core::dynamic_prompt::build::DynamicPromptCoreContext| format!("{:?}", context.surface);
    let mut options = options();
    options.core_prompt = Some(&core);
    options.surface = maho_core::dynamic_prompt::PromptSurface::Chat;
    let prompt = maho_core::dynamic_prompt::build::build_dynamic_system_prompt(&options);
    assert!(prompt.starts_with("Chat\n"));
    assert!(!prompt.contains("Current working directory: /tmp/project"));
}
#[test]
fn user_addenda_are_composed_with_app_surface_presets() {
    let preset = resolve_preset(ModelMetadata { id: "unknown", provider: "fixture", name: None, prompt_preset: None }, PromptPresetName::Gpt55, options()).unwrap();
    assert!(preset.prompt.contains("## Handoff"));
    assert!(preset.prompt.contains("Ask: [the user's original request]"));
}
#[test]
fn explicit_prompt_presets_render_chat_without_a_handoff_ledger() {
    let mut options = options();
    options.surface = maho_core::dynamic_prompt::PromptSurface::Chat;
    let preset = resolve_preset(ModelMetadata { id: "unknown", provider: "fixture", name: None, prompt_preset: None }, PromptPresetName::Gpt55, options).unwrap();
    assert!(preset.prompt.contains("## Replies"));
    assert!(!preset.prompt.contains("Ask: [the user's original request]"));
}
#[test]
fn all_explicit_presets_dispatch_to_nonempty_prompts() {
    use PromptPresetName::*;
    for preset in [ClaudeFable5, ClaudeFable51, ClaudeOpus55, ClaudeOpus5, ClaudeOpus48, ClaudeOpus47, ClaudeOpus46, ClaudeOpus45, DeepseekV4Flash, DeepseekV4Flash0731, DeepseekV41Flash, DeepseekV4Pro, Glm52, Glm53, Grok45, Grok46, Grok47, KimiK3, KimiK28, KimiK27, KimiK26, Gpt5, Gpt52, Gpt53Codex, Gpt54, Gpt55, Gpt56, Gpt6Astra] {
        let resolved = resolve_preset(ModelMetadata { id: "unknown", provider: "fixture", name: None, prompt_preset: None }, preset, options()).unwrap();
        assert_eq!(resolved.name, preset);
        assert!(!resolved.prompt.is_empty());
        assert!(!resolved.prompt.contains("${"));
    }
}
#[test]
fn preset_capture_forwards_the_exact_render_once_and_preserves_context_sources() {
    let observed = std::cell::RefCell::new(Vec::new());
    let observer = |parts: &[maho_ext_api::DynamicSystemPromptPart]| {
        observed.borrow_mut().push(parts.to_vec());
    };
    let mut input = options();
    input.on_parts = Some(&observer);
    input.context_files.push(maho_core::system_prompt::ContextFile {
        path: "/project/AGENTS.md".into(),
        content: "fixture context".into(),
    });
    let resolved = resolve_preset(
        ModelMetadata { id: "unknown", provider: "fixture", name: None, prompt_preset: None },
        PromptPresetName::Gpt55,
        input,
    ).unwrap();
    let observed = observed.into_inner();
    assert_eq!(observed.len(), 1);
    assert_eq!(observed[0].iter().map(|part| part.text.as_str()).collect::<String>(), resolved.prompt);
    assert_eq!(resolved.parts.iter().map(|part| part.text.as_str()).collect::<String>(), resolved.prompt);
    for (original, captured) in observed[0].iter().zip(&resolved.parts) {
        assert_eq!(original.name, captured.name);
        assert_eq!(original.text, captured.text);
        let expected_source = if original.name == "project_context"
            && original.source.as_deref().is_some_and(|source| source != "dynamic-prompt")
        {
            original.source.as_deref()
        } else {
            Some("prompt-preset:gpt-5.5")
        };
        assert_eq!(captured.source.as_deref(), expected_source);
    }
}
#[test]
fn unknown_auto_model_has_no_preset() {
    assert!(resolve_preset(ModelMetadata { id: "unknown", provider: "fixture", name: None, prompt_preset: None }, PromptPresetName::Auto, options()).is_none());
}
