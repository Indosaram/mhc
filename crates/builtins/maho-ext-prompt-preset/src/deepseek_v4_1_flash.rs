use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section};

pub fn build_deepseek_v41_flash_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.tuning_section = Some(build_execution_tooling_section(&options.selected_tools, ExecutionToolingDialect::Claude));
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
