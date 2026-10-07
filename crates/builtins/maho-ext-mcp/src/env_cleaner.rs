//! Port of the pinned `packages/mcp-client-core/src/skill-mcp-manager/env-cleaner.ts`.
//!
//! `createCleanMcpEnvironment` builds the environment a stdio MCP server inherits: the ambient
//! process environment minus `EXCLUDED_ENV_PATTERNS`, then the skill-configured env overlaid
//! unfiltered, so an explicitly declared credential still reaches its server.
use std::collections::BTreeMap;
use std::sync::LazyLock;
use regex::Regex;
/// Pinned `EXCLUDED_ENV_PATTERNS`, order preserved. `(?i)` marks the pinned `/i` patterns; the bare
/// `^npm_config_`, `^YARN_`, `^PNPM_` and `^NO_UPDATE_NOTIFIER$` stay case-sensitive exactly as
/// pinned.
pub const EXCLUDED_ENV_PATTERNS: &[&str] = &[
    r"(?i)^NPM_CONFIG_", r"^npm_config_", r"^YARN_", r"^PNPM_", r"^NO_UPDATE_NOTIFIER$",
    r"(?i)^ANTHROPIC_API_KEY$", r"(?i)^AWS_ACCESS_KEY_ID$", r"(?i)^AWS_SECRET_ACCESS_KEY$",
    r"(?i)^GOOGLE_APPLICATION_CREDENTIALS$", r"(?i)^GOOGLE_CLOUD_PROJECT$", r"(?i)^GITHUB_TOKEN$",
    r"(?i)^DATABASE_URL$", r"(?i)^OPENAI_API_KEY$", r"(?i)^AZURE_", r"(?i)^GCP_",
    r"(?i)^FIREBASE_", r"(?i)^HEROKU_", r"(?i)^DOCKER_AUTH", r"(?i)^KUBECONFIG$", r"(?i)^VAULT_",
    r"(?i)_KEY$", r"(?i)_SECRET$", r"(?i)_TOKEN$", r"(?i)_PASSWORD$", r"(?i)_CREDENTIAL$",
    r"(?i)_CREDENTIALS$", r"(?i)_API_KEY$",
];
static COMPILED:LazyLock<Vec<Regex>> = LazyLock::new(||EXCLUDED_ENV_PATTERNS.iter().copied().map(|pattern|Regex::new(pattern).expect("pinned env-cleaner patterns are literals")).collect());
/// Pinned `env-cleaner.ts::isExcludedEnvKey`.
pub fn is_excluded_env_key(key:&str)->bool {COMPILED.iter().any(|pattern|pattern.is_match(key))}
/// Pinned `env-cleaner.ts::createCleanMcpEnvironment(customEnv)`: the ambient process environment
/// minus `EXCLUDED_ENV_PATTERNS`, then `customEnv` overlaid unfiltered (the pinned
/// `Object.assign(cleanEnv, customEnv)`).
pub fn create_clean_mcp_environment(custom_env:&BTreeMap<String,String>)->BTreeMap<String,String> {create_clean_mcp_environment_from(&ambient_process_environment(),custom_env)}
/// The pinned contract with the ambient snapshot injected. The host session environment is this
/// port's `process.env` (`index.rs` passes `std::env::vars()`), so the production stdio consumer
/// filters that snapshot; tests inject a synthetic snapshot so the assertion is deterministic.
pub fn create_clean_mcp_environment_from(ambient:&BTreeMap<String,String>,custom_env:&BTreeMap<String,String>)->BTreeMap<String,String> {
    let mut clean:BTreeMap<String,String>=ambient.iter().filter(|(key,_)|!is_excluded_env_key(key.as_str())).map(|(key,value)|(key.clone(),value.clone())).collect();
    clean.extend(custom_env.iter().map(|(key,value)|(key.clone(),value.clone())));
    clean
}
fn ambient_process_environment()->BTreeMap<String,String> {std::env::vars_os().map(|(key,value)|(key.to_string_lossy().into_owned(),value.to_string_lossy().into_owned())).collect()}
