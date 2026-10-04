use crate::schema::AskUserVariant;
use maho_ai::apply_patch_wire::{ApplyPatchWireMode, get_apply_patch_wire_mode};
use maho_ext_api::Model;

pub const TOOL_NAMES: [&str; 2] = ["request_user_input", "ask_user_question"];
pub fn pick_variant(model: Option<&Model>) -> AskUserVariant {
    match get_apply_patch_wire_mode(model.map(|model| (model.api.as_str(), model.id.as_str()))) {
        ApplyPatchWireMode::None => AskUserVariant::Claude,
        ApplyPatchWireMode::Json | ApplyPatchWireMode::Freeform => AskUserVariant::Codex,
    }
}
pub const fn tool_name(variant: AskUserVariant) -> &'static str {
    match variant { AskUserVariant::Codex => TOOL_NAMES[0], AskUserVariant::Claude => TOOL_NAMES[1] }
}
