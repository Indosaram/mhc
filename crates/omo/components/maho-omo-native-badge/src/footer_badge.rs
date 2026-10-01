use maho_ext_api::ExtensionUi;

pub const NATIVE_BADGE_STATUS_KEY: &str = "  omo-native";
pub const NATIVE_BADGE_TEXT: &str = "(😺 OmO Native)";

pub struct NativeBadgeStatus;
impl NativeBadgeStatus {
    pub fn publish(&self, ui: Option<&dyn ExtensionUi>) {
        if let Some(ui) = ui { ui.set_status(NATIVE_BADGE_STATUS_KEY, Some(NATIVE_BADGE_TEXT)); }
    }
    pub fn clear(&self, ui: Option<&dyn ExtensionUi>) {
        if let Some(ui) = ui { ui.set_status(NATIVE_BADGE_STATUS_KEY, None); }
    }
}
