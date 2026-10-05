use std::sync::atomic::{AtomicBool, Ordering};

pub const NATIVE_BYPASS_MESSAGE: &str = "Image generation is handled by the provider's native server-side tool for the current model. Call the native image_generation tool instead of generate_image.";
static NATIVE_BYPASS: AtomicBool = AtomicBool::new(false);
static REGISTRY_OVERRIDE: std::sync::OnceLock<std::sync::Mutex<Option<std::sync::Arc<dyn crate::auth::ImageGenAuthRegistry>>>> = std::sync::OnceLock::new();
pub fn set_image_gen_registry_override(registry: Option<std::sync::Arc<dyn crate::auth::ImageGenAuthRegistry>>) {
    *REGISTRY_OVERRIDE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner) = registry;
}
pub fn image_gen_registry_override() -> Option<std::sync::Arc<dyn crate::auth::ImageGenAuthRegistry>> {
    REGISTRY_OVERRIDE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}
pub fn set_native_bypass(enabled: bool) { NATIVE_BYPASS.store(enabled, Ordering::SeqCst); }
pub fn is_native_bypass() -> bool { NATIVE_BYPASS.load(Ordering::SeqCst) }

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bypass_flips_both_directions() {
        set_native_bypass(true);
        assert!(is_native_bypass());
        set_native_bypass(false);
        assert!(!is_native_bypass());
    }
}
