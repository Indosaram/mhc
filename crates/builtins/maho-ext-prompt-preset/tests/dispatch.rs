use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
use maho_ext_prompt_preset::{presets::{ModelMetadata, resolve_preset}, settings::PromptPresetName};
fn options() -> BuildDynamicSystemPromptOptions<'static> {
    BuildDynamicSystemPromptOptions { cwd: "/tmp/project".into(), selected_tools: vec!["eval".into(), "read".into()], tool_snippets: Default::default(), prompt_guidelines: Vec::new(), context_files: Vec::new(), skills: Vec::new(), tuning_section: None, core_prompt: None, workstation_dialect: None }
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
fn unknown_auto_model_has_no_preset() {
    assert!(resolve_preset(ModelMetadata { id: "unknown", provider: "fixture", name: None, prompt_preset: None }, PromptPresetName::Auto, options()).is_none());
}
