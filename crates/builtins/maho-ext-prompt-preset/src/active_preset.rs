//! Port of `prompt-preset/active-preset.ts` (senpi v2026.11.19-5).

use crate::{
    contract::preset_shared_sections,
    index::{has_user_system_prompt, metadata, settings},
    presets::resolve_preset_name,
    settings::PromptPresetName,
};
use maho_core::dynamic_prompt::SharedSections;
use maho_ext_api::{ExtensionContext, Model};

/// The preset this extension renders for `model` (default: the context's model), or `None` when none
/// applies: no model, or a user system prompt (--system-prompt / SDK loader override) outranks presets.
pub fn resolve_active_preset_name(
    ctx: &ExtensionContext,
    model: Option<&Model>,
) -> Option<PromptPresetName> {
    let model = model.or(ctx.model.as_ref())?;
    if has_user_system_prompt(&ctx.get_system_prompt_options()) {
        return None;
    }
    let prompt_preset = ctx.model_registry.prompt_preset(model);
    resolve_preset_name(metadata(model, prompt_preset.as_deref()), settings(ctx))
}

/// The shared-section wording of the active preset (senpi#5665), for extensions that append their own
/// shared prompt text after the preset renders (terminal, todotools, websearch, bash-timeout).
pub fn active_shared_sections(ctx: &ExtensionContext) -> SharedSections {
    preset_shared_sections(resolve_active_preset_name(ctx, None))
}
