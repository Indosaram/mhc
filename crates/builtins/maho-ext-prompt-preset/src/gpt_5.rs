use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};

pub fn build_gpt5_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.tuning_section = Some(["Focus on what \"done\" looks like rather than chaining intermediate confirmations when the goal is already concrete. Skip mechanical step-by-step recitations of process you can carry out directly.\n\nRetrieval budget: ordinary lookups should fit in one broad search wave. Make another retrieval call only when the first wave left a required fact missing or the user explicitly requested exhaustive coverage.\n\n".into(), crate::gpt_eval_routing::build_gpt_eval_routing_tuning().to_owned(), "\n\n".into(), crate::file_operations::build_file_operations_tuning(&options.selected_tools)].concat());
    options.workstation_dialect = Some(WorkstationDialect::Codex);
    build_dynamic_system_prompt(&options)
}
