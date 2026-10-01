use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};

pub fn build_gpt54_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.tuning_section = Some(["Use explicit section structure and step sequences for multi-step tasks — ordered steps with dependencies. When a specific response shape is needed, declare exact fields and order upfront, no extra text.\n\nDefault to medium reasoning effort. Escalate to high only for multi-constraint optimization, subtle bugs, or novel architecture decisions. Use low for classification, extraction, formatting.\n\nState when each tool should and should not be called. Specify parallel vs sequential tool use.\n\n".into(), crate::gpt_eval_routing::build_gpt_eval_routing_tuning().to_owned(), "\n\n".into(), crate::file_operations::build_file_operations_tuning(&options.selected_tools)].concat());
    options.workstation_dialect = Some(WorkstationDialect::Codex);
    build_dynamic_system_prompt(&options)
}
