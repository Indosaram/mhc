pub const ENV_ENABLE_GROK_NEO: &str = "SENPI_ENABLE_GROK_NEO";
pub fn is_grok_neo_enabled(env: &std::collections::HashMap<String, String>) -> bool {
    maho_core::brand::env_value("ENABLE_GROK_NEO", env).is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes"))
}
