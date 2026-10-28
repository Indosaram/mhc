use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
/// Launch baseline retains the current Fable 5.1 core and tuning.
pub fn build_claude_fable55_prompt(options: BuildDynamicSystemPromptOptions<'_>) -> String {
    crate::claude_fable_5_1::build_claude_fable51_prompt(options)
}
