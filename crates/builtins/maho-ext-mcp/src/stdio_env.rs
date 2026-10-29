//! Secret filtering for the host session environment handed to stdio servers.
use std::collections::BTreeMap;

const EXACT_KEYS: &[&str] = &[
    "NO_UPDATE_NOTIFIER",
    "AWS_ACCESS_KEY_ID",
    "AWS_SECRET_ACCESS_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "KUBECONFIG",
    "DATABASE_URL",
    "ANTHROPIC_API_KEY",
    "OPENAI_API_KEY",
    "GITHUB_TOKEN",
];
const PREFIXES: &[&str] = &[
    "NPM_CONFIG_",
    "YARN_",
    "PNPM_",
    "AZURE_",
    "GCP_",
    "FIREBASE_",
    "HEROKU_",
    "VAULT_",
    "DOCKER_AUTH",
];
const SUFFIXES: &[&str] = &[
    "_KEY",
    "_SECRET",
    "_TOKEN",
    "_PASSWORD",
    "_CREDENTIAL",
    "_CREDENTIALS",
    "_API_KEY",
];

/// True when a host environment variable must not reach a stdio server (case-insensitive).
pub fn is_filtered_env_key(key: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    EXACT_KEYS.contains(&upper.as_str())
        || PREFIXES.iter().any(|prefix| upper.starts_with(prefix))
        || SUFFIXES.iter().any(|suffix| upper.ends_with(suffix))
}

/// Drops every filtered key from a host environment map.
pub fn filter_host_env(env: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    env.iter()
        .filter(|(key, _)| !is_filtered_env_key(key))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
