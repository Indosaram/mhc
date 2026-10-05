use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section};

pub fn build_kimi_k26_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    let tooling = build_execution_tooling_section(&options.selected_tools, ExecutionToolingDialect::Kimi);
    let tuning = "Avoid restating the user's request, do not re-derive facts you already established this turn, and skip filler verification language (\"let me confirm again\", \"to be sure\", \"just to double-check\").\n\nThe intent gate routing line is required every turn. On confirmation turns where the user already chose an option in plain words, acknowledge that choice and execute, not re-litigate alternatives the user already eliminated.".to_owned();
    options.tuning_section = Some([tooling.as_str(), tuning.as_str()].into_iter().filter(|section| !section.is_empty()).collect::<Vec<_>>().join("\n\n"));
    options.workstation_dialect = Some(WorkstationDialect::Kimi);
    build_dynamic_system_prompt(&options)
}
