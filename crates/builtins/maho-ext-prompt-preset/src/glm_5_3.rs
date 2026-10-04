use maho_core::dynamic_prompt::build::BuildDynamicSystemPromptOptions;
pub fn build_glm53_prompt(options: BuildDynamicSystemPromptOptions<'_>) -> String { crate::glm_5::build_glm5_prompt(options) }
