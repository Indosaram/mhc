use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::deepseek_v4::build_deepseek_v4_tuning;

const PRO_INTRO: &str = "You are running on DeepSeek V4 Pro - a deep reasoner with a decisive finish. Reasoning depth is a budget spent on the problem, not a ritual: routine classification, file edits, and lookups are decided directly, while hard design and debugging work gets the full depth.";
pub fn build_deepseek_v4_pro_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.tuning_section = Some(build_deepseek_v4_tuning("deepseek-v4-pro", PRO_INTRO));
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
