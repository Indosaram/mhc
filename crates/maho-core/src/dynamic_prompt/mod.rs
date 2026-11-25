//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/index.ts`.

pub mod build;
pub mod handoff;
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

pub use build::{
    BuildDynamicSystemPromptOptions, DynamicPromptCoreContext, DynamicPromptRenderOptions,
    FullToolPrompt, PROMPT_SURFACE_ENV_VAR, build_dynamic_system_prompt,
    build_dynamic_system_prompt_parts_with_options, build_dynamic_system_prompt_with_options,
    resolve_prompt_surface,
};
pub use identity::{build_identity_section, build_identity_section_for_sections};
pub use intent_gate::{build_intent_gate, build_intent_gate_with_options};
pub use policies::{build_policies_section, build_policies_section_with_options};
pub use style::{build_style_section, build_style_section_for_sections};
pub use tool_categorization::{categorize_tools, get_tool_category, get_tools_prompt_display};
pub use tool_section::{
    CATEGORY_ORDER, build_tool_section, build_tool_section_for_sections, category_label,
};
pub use types::{AvailableTool, PromptSurface, SharedSections, ToolCategory};
pub use verification::{
    TEST_DISCIPLINE_RULES, TestDisciplineRule, build_test_discipline_section,
    build_test_discipline_section_for_sections, build_verification_section,
    build_verification_section_for_sections,
};
pub use working_task::{build_working_task_section, build_working_task_section_with_options};
pub use workstation::{
    BuildWorkstationSectionOptions, WorkstationDialect, WorkstationFacts,
    build_workstation_section, collect_workstation_facts, executor_phrase, pinned_display_path,
    read_workstation_facts,
};
