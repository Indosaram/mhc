//! Native equivalent of the API registration triggered by the upstream CLI imports, plus the
//! process environment senpi's entry modules assign at startup.
use std::collections::HashMap;

/// senpi `main.ts` `offlineMode`: `--offline` or a truthy brand-prefixed `OFFLINE`.
pub fn is_offline_mode(argv: &[String], env: &HashMap<String, String>) -> bool {
    argv.iter().any(|arg| arg == "--offline")
        || super::shared_host::is_truthy_env_flag(maho_core::brand::env_value("OFFLINE", env).as_deref())
}

/// senpi `cli.ts`/`cli-main.ts`/`rpc-entry.ts` (`scrubBrandFromEnvironment`, `PI_CODING_AGENT`,
/// `AI_AGENT`) and `main.ts:913-917` (offline mode): the environment the process continues with.
/// The brand profile must already be resolved, since the brand variable is removed here.
pub fn startup_process_env(argv: &[String], env: &HashMap<String, String>) -> HashMap<String, String> {
    let mut next = env.clone();
    maho_core::brand::scrub_brand_from_environment(&mut next);
    next.insert("PI_CODING_AGENT".to_owned(), "true".to_owned());
    next.insert("AI_AGENT".to_owned(), maho_core::config::app_name());
    if is_offline_mode(argv, env) {
        next.insert("PI_OFFLINE".to_owned(), "1".to_owned());
        next.insert("PI_SKIP_VERSION_CHECK".to_owned(), "1".to_owned());
    }
    next
}

/// senpi `main.ts:944-945`: `applyHttpProxySettings(bootstrapSettingsManager.getGlobalSettings().httpProxy)`
/// over settings created with `projectTrusted: false`.
pub fn bootstrap_http_proxy_env(argv: &[String], cwd: &str, agent_dir: &str, home_dir: &str, env: &HashMap<String, String>) -> Result<HashMap<String, String>, String> {
    let settings = maho_core::settings_manager::SettingsManager::create(cwd, agent_dir, home_dir, false);
    let proxy = settings.get_global().get("httpProxy").and_then(serde_json::Value::as_str).map(str::to_owned);
    let mut next = env.clone();
    maho_core::http_dispatcher::apply_http_proxy_settings(proxy.as_deref(), argv.iter().any(|arg| arg == "--multi-session"), &mut next)?;
    Ok(next)
}

pub fn register_builtin_apis() {
    use maho_ai::{api, api_registry::register_builtin_api_provider as register};
    register("anthropic-messages", api::anthropic_messages_lazy::anthropic_messages_api());
    register("azure-openai-responses", api::azure_openai_responses_lazy::azure_openai_responses_api());
    register("bedrock-converse-stream", api::bedrock_converse_stream_lazy::bedrock_converse_stream_api());
    register("cursor-agent", std::sync::Arc::new(api::cursor_agent_lazy::cursor_agent_api()));
    register("devin-agent", api::devin_agent_lazy::devin_agent_api());
    register("google-generative-ai", api::google_generative_ai_lazy::google_generative_ai_api());
    register("google-vertex", api::google_vertex_lazy::google_vertex_api());
    register("mistral-conversations", api::mistral_conversations_lazy::mistral_conversations_api());
    register("openai-codex-responses", api::openai_codex_responses_lazy::openai_codex_responses_api());
    register("openai-completions", api::openai_completions_lazy::open_ai_completions_api());
    register("openai-responses", api::openai_responses_lazy::openai_responses_api());
    register("pi-messages", api::pi_messages_lazy::pi_messages_api());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(key, value)| ((*key).to_owned(), (*value).to_owned())).collect()
    }

    #[test]
    fn startup_env_marks_the_agent_and_scrubs_the_brand() {
        let next = startup_process_env(&[], &env(&[(maho_core::brand::BRAND_ENV_VAR, r#"{"name":"qa"}"#), ("KEEP", "1")]));
        assert_eq!(next.get("PI_CODING_AGENT").map(String::as_str), Some("true"));
        assert_eq!(next.get("AI_AGENT"), Some(&maho_core::config::app_name()));
        assert!(!next.contains_key(maho_core::brand::BRAND_ENV_VAR));
        assert_eq!(next.get("KEEP").map(String::as_str), Some("1"));
        assert!(!next.contains_key("PI_OFFLINE"));
    }

    #[test]
    fn offline_flag_disables_network_and_version_check() {
        let next = startup_process_env(&["--offline".to_owned()], &HashMap::new());
        assert_eq!(next.get("PI_OFFLINE").map(String::as_str), Some("1"));
        assert_eq!(next.get("PI_SKIP_VERSION_CHECK").map(String::as_str), Some("1"));
    }

    #[test]
    fn truthy_offline_env_enables_offline_mode() {
        assert!(is_offline_mode(&[], &env(&[("PI_OFFLINE", "yes")])));
        assert!(!is_offline_mode(&[], &env(&[("PI_OFFLINE", "0")])));
    }

    #[test]
    fn bootstrap_proxy_fills_unset_proxy_variables_only() {
        let root = tempfile::tempdir().expect("temp root");
        let agent = root.path().join("agent");
        std::fs::create_dir_all(&agent).expect("agent dir");
        std::fs::write(agent.join("settings.json"), r#"{"httpProxy":" http://proxy.local:8080 "}"#).expect("settings");
        let (cwd, agent, home) = (root.path().to_string_lossy(), agent.to_string_lossy(), root.path().to_string_lossy());
        let next = bootstrap_http_proxy_env(&[], &cwd, &agent, &home, &env(&[("HTTPS_PROXY", "http://existing:1")])).expect("proxy applied");
        assert_eq!(next.get("HTTP_PROXY").map(String::as_str), Some("http://proxy.local:8080"));
        assert_eq!(next.get("HTTPS_PROXY").map(String::as_str), Some("http://existing:1"));
    }
}
