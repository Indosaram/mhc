//! Port of `prompt-preset/generation.ts` (senpi 903b8336): the stable entry points other crates use to
//! render a preset without reaching into the per-model modules.

pub use maho_core::dynamic_prompt::build::{
    BuildDynamicSystemPromptOptions, build_dynamic_system_prompt,
};

use crate::{
    presets::{ModelMetadata, ResolvedPromptPreset, resolve_preset as native_resolve},
    settings::PromptPresetName,
};

/// `buildPreset(name, options)`: renders the named preset. `None` only for `Auto`, which upstream's
/// `ResolvedPresetName` excludes and which has no prompt of its own.
pub fn build_preset(
    name: PromptPresetName,
    options: BuildDynamicSystemPromptOptions<'_>,
) -> Option<ResolvedPromptPreset> {
    if name == PromptPresetName::Auto {
        return None;
    }
    // A non-auto setting resolves to itself without reading the model.
    let model = ModelMetadata {
        id: "",
        provider: "",
        name: None,
        prompt_preset: None,
    };
    native_resolve(model, name, options)
}

/// `resolvePreset(model, settings, options)`.
pub fn resolve_preset(
    model: ModelMetadata<'_>,
    setting: PromptPresetName,
    options: BuildDynamicSystemPromptOptions<'_>,
) -> Option<ResolvedPromptPreset> {
    native_resolve(model, setting, options)
}
