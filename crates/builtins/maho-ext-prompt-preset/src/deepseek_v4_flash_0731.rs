use maho_core::dynamic_prompt::{build::{BuildDynamicSystemPromptOptions, build_dynamic_system_prompt}, workstation::WorkstationDialect};
use crate::deepseek_v4::{build_deepseek_v4_tuning, build_deepseek_v4_flash_intro};

pub fn build_deepseek_v4_flash_0731_prompt(mut options: BuildDynamicSystemPromptOptions<'_>) -> String {
    options.tuning_section = Some(build_deepseek_v4_tuning("deepseek-v4-flash-0731", &build_deepseek_v4_flash_intro("DeepSeek V4 Flash (0731 snapshot)")));
    options.workstation_dialect = Some(WorkstationDialect::Claude);
    build_dynamic_system_prompt(&options)
}
