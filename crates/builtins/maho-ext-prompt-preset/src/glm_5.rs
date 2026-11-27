use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section};
use maho_core::dynamic_prompt::{
    build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt},
    workstation::WorkstationDialect,
};

pub const GLM5_TUNING: &str = "Cheap tool call beats long internal debate: reading/running/searching can settle it -> do that, reason over result. Short act-inspect-verify loops so early mistakes surface before later steps build on them. Only the user can settle it -> ask_user_question when available.";
pub fn build_glm5_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    let tooling = build_execution_tooling_section(
        &options.selected_tools,
        ExecutionToolingDialect::Claude,
        options.shared_sections,
    );
    options.tuning_section = Some(
        [tooling.as_str(), GLM5_TUNING]
            .into_iter()
            .filter(|section| !section.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
    );
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
