use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::execution_tooling::{ExecutionToolingDialect, build_execution_tooling_section};

pub fn build_kimi_k2_code_prompt(mut options: BuildDynamicSystemPromptOptions<'_>, model_name: &str) -> String {
    let tooling = build_execution_tooling_section(&options.selected_tools, ExecutionToolingDialect::Kimi);
    let tuning = format!("You are running on {model_name} - restrained and outcome-first. Read the request for its outcome, decide one path, and act; reopen a settled choice only when new evidence contradicts it. Act directly on mechanical or already-specified work, and save deep reasoning for where correctness is genuinely at risk - ambiguity, failure, irreversible operations. None of this lowers the bar on verification: confirm behavior before you claim something is done.\n\nThe intent gate routing line is required every turn. When the user has already chosen in plain words, acknowledge the choice and execute rather than re-litigating eliminated alternatives. Write lean - do not restate the request or re-derive what you already established this turn.");
    options.tuning_section = Some([tooling.as_str(), tuning.as_str()].into_iter().filter(|section| !section.is_empty()).collect::<Vec<_>>().join("\n\n"));
    options.workstation_dialect = Some(WorkstationDialect::Kimi);
    build_dynamic_system_prompt(&options)
}
