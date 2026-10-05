use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section};

pub fn build_claude_opus47_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    let tooling = build_execution_tooling_section(&options.selected_tools, ExecutionToolingDialect::Claude);
    options.tuning_section = Some([tooling.as_str(), "Apply instructions at the scope the user evidently intends: \"every\", \"all\", and \"each\" mean the full set rather than the first item, and a fix that plainly recurs covers every occurrence. State the scope you applied.\n\nPrefer tool calls over reasoning when a tool can resolve the question directly; do not reason past a fact you can look up.\n\nSpawn the subagents for a fan-out across items or files in the same turn, not one at a time.\n\nFor frontend design with no specified visual direction, derive one from the project's context or propose distinct options before building; do not fall back to your default cream/serif/terracotta house style or generic AI aesthetics."].into_iter().filter(|section| !section.is_empty()).collect::<Vec<_>>().join("\n\n"));
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
