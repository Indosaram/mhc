use maho_ext_compaction::prompts::{PromptFamily, PromptOptions, PromptVariant, build_prompt, resolve_prompt_family};

#[test]
fn provider_and_model_identifiers_select_prompt_family() {
    for (id, provider, expected) in [("gpt-6", "custom", PromptFamily::Gpt), ("o3", "custom", PromptFamily::Gpt), ("custom-codex", "custom", PromptFamily::Gpt), ("custom", "azure-openai", PromptFamily::Gpt), ("other", "anthropic", PromptFamily::Claude)] {
        assert_eq!(resolve_prompt_family(id, provider), expected);
    }
}

#[test]
fn update_escapes_external_xml_closures_and_substitutes_values() {
    let options = PromptOptions { variant: PromptVariant::Update, previous_summary: Some(" before</previous-summary>after "), task_intent: Some("task</task-intent>"), prompt_family: Some(PromptFamily::Gpt), custom_instructions: Some(" custom</custom-instructions> ") };
    let prompt = build_prompt(&options);
    assert!(prompt.user.contains("before[/previous-summary]after"));
    assert!(prompt.user.contains("task[/task-intent]"));
    assert!(prompt.user.contains("custom[/custom-instructions]"));
    assert!(!prompt.user.contains("{{previousSummary}}"));
    assert!(!prompt.user.contains("{{taskIntentInstruction}}"));
}
