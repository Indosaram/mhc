pub const STALE_EXTENSION_CONTEXT_ERROR_PREFIX: &str = "This extension ctx is stale after session replacement or reload.";
pub fn is_stale_extension_context_error(error: &dyn std::error::Error) -> bool { error.to_string().starts_with(STALE_EXTENSION_CONTEXT_ERROR_PREFIX) }
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn stale_context_error_is_recognized() { let error = maho_ext_api::ExtensionFailure::new(format!("{STALE_EXTENSION_CONTEXT_ERROR_PREFIX} retired")); let result = is_stale_extension_context_error(&error); assert!(result); }
    #[test] fn unrelated_error_does_not_retire_context() { let error = maho_ext_api::ExtensionFailure::new("render failed"); let result = is_stale_extension_context_error(&error); assert!(!result); }
}
