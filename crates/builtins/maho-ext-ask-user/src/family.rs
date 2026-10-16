use crate::schema::AskUserVariant;
use maho_ai::apply_patch_wire::get_apply_patch_wire_mode;
use maho_ext_api::Model;

pub const TOOL_NAMES: [&str; 2] = ["request_user_input", "ask_user_question"];
pub fn pick_variant(model: Option<&Model>) -> AskUserVariant {
    if model.is_some_and(|model| matches!(get_apply_patch_wire_mode(Some((model.api.as_str(), model.id.as_str()))),maho_ai::apply_patch_wire::ApplyPatchWireMode::Json|maho_ai::apply_patch_wire::ApplyPatchWireMode::Freeform)) { AskUserVariant::Codex } else { AskUserVariant::Claude }
}
pub const fn tool_name(variant: AskUserVariant) -> &'static str {
    match variant { AskUserVariant::Codex => TOOL_NAMES[0], AskUserVariant::Claude => TOOL_NAMES[1] }
}
