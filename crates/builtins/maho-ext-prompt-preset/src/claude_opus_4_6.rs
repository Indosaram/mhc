use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section};

pub fn build_claude_opus46_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    let tooling = build_execution_tooling_section(&options.selected_tools, ExecutionToolingDialect::Claude);
    options.tuning_section = Some([tooling.as_str(), ""].into_iter().filter(|section| !section.is_empty()).collect::<Vec<_>>().join("\n\n"));
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
