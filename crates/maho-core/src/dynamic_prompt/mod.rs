//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/index.ts`.

pub mod build;
pub mod identity;
pub mod intent_gate;
pub mod policies;
pub mod style;
pub mod tool_categorization;
pub mod tool_section;
pub mod types;
pub mod verification;
pub mod working_task;
pub mod workstation;

pub use build::{BuildDynamicSystemPromptOptions, DynamicPromptCoreContext, build_dynamic_system_prompt};
pub use identity::build_identity_section;
pub use intent_gate::build_intent_gate;
pub use policies::build_policies_section;
pub use style::build_style_section;
pub use tool_categorization::{categorize_tools, get_tool_category, get_tools_prompt_display};
pub use tool_section::{CATEGORY_ORDER, build_tool_section, category_label};
pub use types::{AvailableTool, ToolCategory};
pub use verification::{TEST_DISCIPLINE_RULES, TestDisciplineRule, build_test_discipline_section, build_verification_section};
pub use working_task::build_working_task_section;
pub use workstation::{
    BuildWorkstationSectionOptions, WorkstationDialect, WorkstationFacts, build_workstation_section,
    collect_workstation_facts, executor_phrase,
};
