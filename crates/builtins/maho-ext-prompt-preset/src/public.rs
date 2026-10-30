//! Port of `prompt-preset/public.ts` (senpi 903b8336): the preset surface other crates import.

pub use crate::contract::*;
pub use crate::generation::{
    BuildDynamicSystemPromptOptions, build_dynamic_system_prompt, build_preset, resolve_preset,
};
pub use crate::matchers::ModelWithPromptPresetMetadata;
pub use crate::presets::ResolvedPromptPreset;
pub use crate::settings::PromptPresetName;
pub use maho_core::dynamic_prompt::PromptSurface;
