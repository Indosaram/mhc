use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};

pub fn build_gpt52_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.tuning_section = Some(["Constrain verbosity explicitly: \"3-6 sentences\", \"max 5 bullets\", \"no preamble\". Do not over-explain simple tasks.\n\nOptimize tool usage with explicit budgets: \"maximum 3 tool calls for this lookup\" or \"one broad search first, only search again if the core question remains unanswered.\"\n\nImplement EXACTLY and ONLY what was requested. No extra features, no scope drift.\n\nCompact after major milestones, not every turn. Keep the system prompt functionally identical when resuming.\n\n".into(), crate::gpt_eval_routing::build_gpt_eval_routing_tuning().to_owned(), "\n\n".into(), crate::file_operations::build_file_operations_tuning(&options.selected_tools)].concat());
    options.workstation_dialect = Some(WorkstationDialect::Codex);
    build_dynamic_system_prompt(&options)
}
