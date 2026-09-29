//! Port of senpi packages/ai/src/env-api-keys.ts.
//!
//! Every lookup goes through an injectable `EnvLookup`; the public functions resolve scoped
//! overrides first, then the process environment (`get_provider_env_value`). The TS module-level
//! Vertex ADC cache exists only to paper over async `node:fs` loading and is not needed here.

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;
use std::path::Path;

pub const ANTHROPIC_AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
pub const ANTHROPIC_OAUTH_TOKEN_ENV: &str = "ANTHROPIC_OAUTH_TOKEN";
pub const ANTHROPIC_API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Resolves one env var name to a non-empty value.
pub type EnvLookup<'a> = &'a dyn Fn(&str) -> Option<String>;

const ENV_MAP: &[(&str, &str)] = &[
    ("alibaba-token-plan", "ALIBABA_TOKEN_PLAN_API_KEY"),
    ("ant-ling", "ANT_LING_API_KEY"),
    ("qwen-token-plan", "QWEN_TOKEN_PLAN_API_KEY"),
    ("qwen-token-plan-cn", "QWEN_TOKEN_PLAN_CN_API_KEY"),
    ("qwen-token-plan-individual", "QWEN_TOKEN_PLAN_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("ollama", "OLLAMA_API_KEY"),
    ("azure-openai-responses", "AZURE_OPENAI_API_KEY"),
    ("nvidia", "NVIDIA_API_KEY"),
    ("deepseek", "DEEPSEEK_API_KEY"),
    ("google", "GEMINI_API_KEY"),
    ("google-vertex", "GOOGLE_CLOUD_API_KEY"),
    ("groq", "GROQ_API_KEY"),
    ("cerebras", "CEREBRAS_API_KEY"),
    ("venice", "VENICE_API_KEY"),
    ("xai", "XAI_API_KEY"),
    ("radius", "RADIUS_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
    ("vercel-ai-gateway", "AI_GATEWAY_API_KEY"),
    ("opengateway", "OPENGATEWAY_API_KEY"),
    ("zai", "ZAI_API_KEY"),
    ("zai-coding-cn", "ZAI_CODING_CN_API_KEY"),
    ("mistral", "MISTRAL_API_KEY"),
    ("minimax", "MINIMAX_API_KEY"),
    ("minimax-cn", "MINIMAX_CN_API_KEY"),
    ("moonshotai", "MOONSHOT_API_KEY"),
    ("moonshotai-cn", "MOONSHOT_API_KEY"),
    ("huggingface", "HF_TOKEN"),
    ("fireworks", "FIREWORKS_API_KEY"),
    ("together", "TOGETHER_API_KEY"),
    ("baseten", "BASETEN_API_KEY"),
    ("bai", "BAI_API_KEY"),
    ("opencode", "OPENCODE_API_KEY"),
    ("opencode-go", "OPENCODE_API_KEY"),
    ("kimi-coding", "KIMI_API_KEY"),
    ("cloudflare-workers-ai", "CLOUDFLARE_API_KEY"),
    ("cloudflare-ai-gateway", "CLOUDFLARE_API_KEY"),
    ("xiaomi", "XIAOMI_API_KEY"),
    ("xiaomi-token-plan-cn", "XIAOMI_TOKEN_PLAN_CN_API_KEY"),
    ("xiaomi-token-plan-ams", "XIAOMI_TOKEN_PLAN_AMS_API_KEY"),
    ("xiaomi-token-plan-sgp", "XIAOMI_TOKEN_PLAN_SGP_API_KEY"),
];

/// Env var names that can carry a provider's API key, in lookup order. Only `COPILOT_GITHUB_TOKEN`
/// counts for GitHub Copilot: generic GitHub tokens are not Copilot credentials.
pub fn get_api_key_env_vars(provider: &str) -> Option<Vec<&'static str>> {
    if provider == "github-copilot" {
        return Some(vec!["COPILOT_GITHUB_TOKEN"]);
    }
    if provider == "anthropic" {
        return Some(vec![ANTHROPIC_AUTH_TOKEN_ENV, ANTHROPIC_OAUTH_TOKEN_ENV, ANTHROPIC_API_KEY_ENV]);
    }
    ENV_MAP.iter().find(|(id, _)| *id == provider).map(|(_, var)| vec![*var])
}

fn process_lookup(env: Option<&ProviderEnv>) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| get_provider_env_value(name, env)
}

pub fn find_env_keys_with(provider: &str, lookup: EnvLookup<'_>) -> Option<Vec<String>> {
    let found: Vec<String> = get_api_key_env_vars(provider)?
        .into_iter()
        .filter(|var| lookup(var).is_some())
        .map(str::to_owned)
        .collect();
    if found.is_empty() { None } else { Some(found) }
}

/// Env var names that currently hold a value for the provider.
pub fn find_env_keys(provider: &str, env: Option<&ProviderEnv>) -> Option<Vec<String>> {
    find_env_keys_with(provider, &process_lookup(env))
}

fn has_vertex_adc_credentials(env: Option<&ProviderEnv>, lookup: EnvLookup<'_>) -> bool {
    if let Some(explicit) = env.and_then(|env| env.get("GOOGLE_APPLICATION_CREDENTIALS")).filter(|p| !p.is_empty()) {
        return Path::new(explicit).exists();
    }
    if let Some(path) = lookup("GOOGLE_APPLICATION_CREDENTIALS") {
        return Path::new(&path).exists();
    }
    dirs::home_dir()
        .map(|home| home.join(".config").join("gcloud").join("application_default_credentials.json"))
        .is_some_and(|path| path.exists())
}

pub fn get_env_api_key_with(provider: &str, env: Option<&ProviderEnv>, lookup: EnvLookup<'_>) -> Option<String> {
    if let Some(keys) = find_env_keys_with(provider, lookup) {
        let api_key_env = if provider == "anthropic" {
            keys.iter().find(|key| key.as_str() != ANTHROPIC_AUTH_TOKEN_ENV)
        } else {
            keys.first()
        };
        if let Some(var) = api_key_env {
            return lookup(var);
        }
    }

    if provider == "google-vertex" {
        let has_project = lookup("GOOGLE_CLOUD_PROJECT").is_some() || lookup("GCLOUD_PROJECT").is_some();
        let has_location = lookup("GOOGLE_CLOUD_LOCATION").is_some();
        if has_vertex_adc_credentials(env, lookup) && has_project && has_location {
            return Some("<authenticated>".into());
        }
    }

    if provider == "amazon-bedrock"
        && (lookup("AWS_PROFILE").is_some()
            || (lookup("AWS_ACCESS_KEY_ID").is_some() && lookup("AWS_SECRET_ACCESS_KEY").is_some())
            || lookup("AWS_BEARER_TOKEN_BEDROCK").is_some()
            || lookup("AWS_CONTAINER_CREDENTIALS_RELATIVE_URI").is_some()
            || lookup("AWS_CONTAINER_CREDENTIALS_FULL_URI").is_some()
            || lookup("AWS_WEB_IDENTITY_TOKEN_FILE").is_some())
    {
        return Some("<authenticated>".into());
    }

    None
}

/// API key for a provider from known environment variables. `ANTHROPIC_AUTH_TOKEN` is reported by
/// `find_env_keys` but is never returned as an Anthropic API key.
pub fn get_env_api_key(provider: &str, env: Option<&ProviderEnv>) -> Option<String> {
    get_env_api_key_with(provider, env, &process_lookup(env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect()
    }

    fn lookup_in(map: &BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
        |name| map.get(name).filter(|v| !v.is_empty()).cloned()
    }

    fn keys(provider: &str, map: &BTreeMap<String, String>) -> Option<Vec<String>> {
        find_env_keys_with(provider, &lookup_in(map))
    }

    fn api_key(provider: &str, map: &BTreeMap<String, String>) -> Option<String> {
        get_env_api_key_with(provider, None, &lookup_in(map))
    }

    #[test]
    fn does_not_treat_generic_github_tokens_as_github_copilot_credentials() {
        let map = env(&[("GH_TOKEN", "gh-token"), ("GITHUB_TOKEN", "github-token")]);
        assert_eq!(keys("github-copilot", &map), None);
        assert_eq!(api_key("github-copilot", &map), None);
    }

    #[test]
    fn resolves_github_copilot_credentials_from_copilot_github_token() {
        let map = env(&[("COPILOT_GITHUB_TOKEN", "copilot-token"), ("GH_TOKEN", "gh-token"), ("GITHUB_TOKEN", "github-token")]);
        assert_eq!(keys("github-copilot", &map), Some(vec!["COPILOT_GITHUB_TOKEN".into()]));
        assert_eq!(api_key("github-copilot", &map).as_deref(), Some("copilot-token"));
    }

    #[test]
    fn resolves_zai_china_coding_plan_credentials_from_zai_coding_cn_api_key() {
        let map = env(&[("ZAI_CODING_CN_API_KEY", "zai-coding-cn-key")]);
        assert_eq!(keys("zai-coding-cn", &map), Some(vec!["ZAI_CODING_CN_API_KEY".into()]));
        assert_eq!(api_key("zai-coding-cn", &map).as_deref(), Some("zai-coding-cn-key"));
    }

    #[test]
    fn resolves_ollama_cloud_credentials_from_ollama_api_key() {
        let map = env(&[("OLLAMA_API_KEY", "ollama-key")]);
        assert_eq!(keys("ollama", &map), Some(vec!["OLLAMA_API_KEY".into()]));
        assert_eq!(api_key("ollama", &map).as_deref(), Some("ollama-key"));
    }

    #[test]
    fn resolves_bai_credentials_from_bai_api_key() {
        let map = env(&[("BAI_API_KEY", "bai-key")]);
        assert_eq!(keys("bai", &map), Some(vec!["BAI_API_KEY".into()]));
        assert_eq!(api_key("bai", &map).as_deref(), Some("bai-key"));
    }

    #[test]
    fn reports_anthropic_auth_token_but_preserves_oauth_token_api_key_lookup() {
        let map = env(&[("ANTHROPIC_AUTH_TOKEN", "auth-token"), ("ANTHROPIC_OAUTH_TOKEN", "oauth-token")]);
        assert_eq!(keys("anthropic", &map), Some(vec!["ANTHROPIC_AUTH_TOKEN".into(), "ANTHROPIC_OAUTH_TOKEN".into()]));
        assert_eq!(api_key("anthropic", &map).as_deref(), Some("oauth-token"));
    }

    #[test]
    fn does_not_return_anthropic_auth_token_as_an_api_key() {
        let map = env(&[("ANTHROPIC_AUTH_TOKEN", "auth-token")]);
        assert_eq!(keys("anthropic", &map), Some(vec!["ANTHROPIC_AUTH_TOKEN".into()]));
        assert_eq!(api_key("anthropic", &map), None);
    }

    #[test]
    fn preserves_anthropic_oauth_token_as_an_api_key() {
        let map = env(&[("ANTHROPIC_OAUTH_TOKEN", "oauth-token")]);
        assert_eq!(keys("anthropic", &map), Some(vec!["ANTHROPIC_OAUTH_TOKEN".into()]));
        assert_eq!(api_key("anthropic", &map).as_deref(), Some("oauth-token"));
    }

    #[test]
    fn falls_back_to_anthropic_api_key_for_api_key_lookup() {
        let map = env(&[("ANTHROPIC_API_KEY", "api-key")]);
        assert_eq!(api_key("anthropic", &map).as_deref(), Some("api-key"));
    }

    #[test]
    fn bedrock_and_vertex_ambient_credentials() {
        assert_eq!(api_key("amazon-bedrock", &env(&[("AWS_PROFILE", "p")])).as_deref(), Some("<authenticated>"));
        assert_eq!(api_key("amazon-bedrock", &env(&[("AWS_ACCESS_KEY_ID", "a")])), None);
        let dir = tempfile::tempdir().expect("tempdir");
        let creds = dir.path().join("adc.json");
        std::fs::write(&creds, "{}").expect("write");
        let path = creds.to_string_lossy().into_owned();
        let scoped = env(&[("GOOGLE_APPLICATION_CREDENTIALS", &path)]);
        let map = env(&[("GOOGLE_CLOUD_PROJECT", "p"), ("GOOGLE_CLOUD_LOCATION", "l")]);
        assert_eq!(get_env_api_key_with("google-vertex", Some(&scoped), &lookup_in(&map)).as_deref(), Some("<authenticated>"));
        let missing = env(&[("GOOGLE_APPLICATION_CREDENTIALS", "/nonexistent/maho-adc.json")]);
        assert_eq!(get_env_api_key_with("google-vertex", Some(&missing), &lookup_in(&map)), None);
        assert_eq!(get_api_key_env_vars("unknown-provider"), None);
    }

    #[test]
    fn scoped_env_resolves_through_public_api() {
        let scoped = env(&[("MAHO_AI_UNUSED", "x")]);
        assert_eq!(find_env_keys("unknown-provider", Some(&scoped)), None);
        let scoped = env(&[("BAI_API_KEY", "scoped-bai")]);
        assert_eq!(get_env_api_key("bai", Some(&scoped)).as_deref(), Some("scoped-bai"));
    }
}
