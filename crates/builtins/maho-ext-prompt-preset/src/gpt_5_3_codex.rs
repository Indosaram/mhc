use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};

pub fn build_gpt53_codex_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.tuning_section = Some(["Bias hard toward action. Implement directly with reasonable assumptions rather than stopping to ask. Do not produce upfront plans or preambles before acting — start working immediately.\n\nDo not re-state the goal between steps. When a milestone completes, move to the next without summarizing unless the user asked for a summary.\n\nAfter compaction, continue from the current state rather than re-deriving prior conclusions. Treat compacted items as opaque.\n\n".into(), crate::gpt_eval_routing::build_gpt_eval_routing_tuning().to_owned(), "\n\n".into(), crate::file_operations::build_file_operations_tuning(&options.selected_tools)].concat());
    options.workstation_dialect = Some(WorkstationDialect::Codex);
    build_dynamic_system_prompt(&options)
}
