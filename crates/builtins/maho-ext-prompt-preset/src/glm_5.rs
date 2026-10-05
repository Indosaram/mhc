use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section};

pub const GLM5_TUNING: &str = "A cheap tool call beats long internal debate: when reading, running, or searching can settle a question, do that and reason over the result. Work in short act-inspect-verify loops so an early mistake surfaces before later steps build on it; when only the user can settle a question, ask through ask_user_question when it is available (waitForAnswer true when the next step depends on the answer).";
pub fn build_glm5_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    let tooling = build_execution_tooling_section(&options.selected_tools, ExecutionToolingDialect::Claude);
    options.tuning_section = Some([tooling.as_str(), GLM5_TUNING].into_iter().filter(|section| !section.is_empty()).collect::<Vec<_>>().join("\n\n"));
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
