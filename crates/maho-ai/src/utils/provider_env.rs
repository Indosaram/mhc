//! Port of senpi packages/ai/src/utils/provider-env.ts.
//!
//! The Bun `/proc/self/environ` fallback exists for a Bun-compiled-binary bug and has no Rust analogue;
//! the process environment is read directly.

use crate::types::ProviderEnv;

/// Resolve a provider env value from scoped overrides, then the process environment. Empty values count as unset.
pub fn get_provider_env_value(name: &str, env: Option<&ProviderEnv>) -> Option<String> {
    if let Some(value) = env.and_then(|env| env.get(name)).filter(|v| !v.is_empty()) {
        return Some(value.clone());
    }
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_value_wins_and_empty_falls_through() {
        let mut env = ProviderEnv::new();
        env.insert("MAHO_AI_TEST_PROVIDER_ENV_SCOPED".into(), "scoped".into());
        assert_eq!(get_provider_env_value("MAHO_AI_TEST_PROVIDER_ENV_SCOPED", Some(&env)).as_deref(), Some("scoped"));
        env.insert("MAHO_AI_TEST_PROVIDER_ENV_EMPTY".into(), String::new());
        assert_eq!(get_provider_env_value("MAHO_AI_TEST_PROVIDER_ENV_EMPTY", Some(&env)), None);
        assert_eq!(get_provider_env_value("PATH", None), std::env::var("PATH").ok().filter(|v| !v.is_empty()));
    }
}
