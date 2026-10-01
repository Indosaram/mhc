use std::{collections::BTreeMap, path::PathBuf};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport { #[default] Stdio, Http }
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Lifecycle { #[default] Lazy, Eager, KeepAlive }
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Exposure { #[default] Auto, Direct, Search, Proxy }
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel { Debug, #[default] Info, Notice, Warning, Error, Critical, Alert, Emergency }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Auth { Mode(AuthMode), Disabled(bool) }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthMode { Bearer, Oauth }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OAuthFlow { Code, ClientCredentials }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OAuthConfig {
    #[serde(skip_serializing_if = "Option::is_none")] pub client_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub callback_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")] pub scopes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub client_metadata_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub flow: Option<OAuthFlow>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DirectTools { All(bool), Patterns(Vec<String>) }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerConfigWire {
    #[serde(rename = "type", skip_serializing_if = "Option::is_none")] pub transport: Option<Transport>,
    #[serde(skip_serializing_if = "Option::is_none")] pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub args: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub env: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub headers: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub auth: Option<Auth>,
    #[serde(skip_serializing_if = "Option::is_none")] pub bearer_token_env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub oauth: Option<OAuthConfig>,
    #[serde(skip_serializing_if = "Option::is_none")] pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")] pub lifecycle: Option<Lifecycle>,
    #[serde(skip_serializing_if = "Option::is_none")] pub idle_timeout_min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub request_timeout_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub connect_timeout_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub startup_timeout_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub include_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub exclude_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub direct_tools: Option<DirectTools>,
    #[serde(skip_serializing_if = "Option::is_none")] pub exposure: Option<Exposure>,
    #[serde(skip_serializing_if = "Option::is_none")] pub log_level: Option<LogLevel>,
}
pub type McpServerConfig = ServerConfigWire;
impl From<&maho_ext_api::McpServerDeclaration> for ServerConfigWire {
    fn from(value: &maho_ext_api::McpServerDeclaration) -> Self {
        use maho_ext_api::{McpAuth, McpDirectTools, McpExposure, McpLifecycle, McpLogLevel, McpTransport};
        Self {
            transport: value.transport.map(|v| match v { McpTransport::Stdio => Transport::Stdio, McpTransport::Http => Transport::Http }),
            url: value.url.clone(), command: value.command.clone(), args: value.args.clone(), env: value.env.clone(), cwd: value.cwd.clone(), headers: value.headers.clone(),
            auth: value.auth.map(|v| match v { McpAuth::Bearer => Auth::Mode(AuthMode::Bearer), McpAuth::OAuth => Auth::Mode(AuthMode::Oauth), McpAuth::Disabled => Auth::Disabled(false) }),
            bearer_token_env: value.bearer_token_env.clone(),
            oauth: value.oauth.as_ref().map(|v| OAuthConfig { client_id: v.client_id.clone(), callback_port: v.callback_port, scopes: v.scopes.clone(), client_metadata_url: v.client_metadata_url.clone(), flow: v.flow.map(|f| match f { maho_ext_api::OAuthFlow::Code => OAuthFlow::Code, maho_ext_api::OAuthFlow::ClientCredentials => OAuthFlow::ClientCredentials }) }),
            enabled: value.enabled,
            lifecycle: value.lifecycle.map(|v| match v { McpLifecycle::Lazy => Lifecycle::Lazy, McpLifecycle::Eager => Lifecycle::Eager, McpLifecycle::KeepAlive => Lifecycle::KeepAlive }),
            idle_timeout_min: value.idle_timeout_min, request_timeout_ms: value.request_timeout_ms, connect_timeout_ms: value.connect_timeout_ms, startup_timeout_ms: value.startup_timeout_ms,
            include_tools: value.include_tools.clone(), exclude_tools: value.exclude_tools.clone(),
            direct_tools: value.direct_tools.as_ref().map(|v| match v { McpDirectTools::Enabled(b) => DirectTools::All(*b), McpDirectTools::Names(items) => DirectTools::Patterns(items.clone()) }),
            exposure: value.exposure.map(|v| match v { McpExposure::Auto => Exposure::Auto, McpExposure::Direct => Exposure::Direct, McpExposure::Search => Exposure::Search, McpExposure::Proxy => Exposure::Proxy }),
            log_level: value.log_level.map(|v| match v { McpLogLevel::Debug => LogLevel::Debug, McpLogLevel::Info => LogLevel::Info, McpLogLevel::Notice => LogLevel::Notice, McpLogLevel::Warning => LogLevel::Warning, McpLogLevel::Error => LogLevel::Error, McpLogLevel::Critical => LogLevel::Critical, McpLogLevel::Alert => LogLevel::Alert, McpLogLevel::Emergency => LogLevel::Emergency }),
        }
    }
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutputGuardSettings {
    #[serde(skip_serializing_if = "Option::is_none")] pub max_bytes: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub max_lines: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub max_tokens: Option<f64>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum NativeToolSearch { Auto(String), Enabled(bool) }
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct McpSettings {
    #[serde(skip_serializing_if = "Option::is_none")] pub tool_prefix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub search_threshold: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub output_guard: Option<OutputGuardSettings>,
    #[serde(skip_serializing_if = "Option::is_none")] pub import_configs: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")] pub oauth_callback_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub stub_swap: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")] pub native_tool_search: Option<NativeToolSearch>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RawConfig {
    #[serde(skip_serializing_if = "Option::is_none")] pub settings: Option<McpSettings>,
    #[serde(skip_serializing_if = "Option::is_none")] pub mcp_servers: Option<BTreeMap<String, ServerConfigWire>>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpServerSource { Global, Claude, Project, Skill, Extension }
impl std::fmt::Display for McpServerSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self { Self::Global => "global", Self::Claude => "claude", Self::Project => "project", Self::Skill => "skill", Self::Extension => "extension" })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum McpServerState { Enabled, Disabled, Untrusted }
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedMcpServer {
    pub name: String, pub source: McpServerSource, pub source_path: PathBuf,
    pub state: McpServerState, pub transport: Option<Transport>, pub config_hash: Option<String>, pub config: Option<McpServerConfig>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedMcpConfig {
    pub settings: McpSettings, pub servers: BTreeMap<String, ResolvedMcpServer>, pub diagnostics: Vec<String>,
}
pub struct LoadMcpConfigOptions<'a> {
    pub cwd: &'a std::path::Path, pub agent_dir: &'a std::path::Path,
    pub env: &'a BTreeMap<String, String>, pub project_trusted: bool,
}
pub fn get_server_endpoint_validation_error(config: &RawConfig) -> Option<String> {
    for (name, server) in config.mcp_servers.iter().flat_map(|servers| servers.iter()) {
        if server.enabled == Some(false) { continue; }
        let transport = server.transport.unwrap_or(if server.url.as_ref().is_some_and(|s| !s.is_empty()) { Transport::Http } else { Transport::Stdio });
        let (field, value, kind) = match transport { Transport::Stdio => ("command", &server.command, "stdio"), Transport::Http => ("url", &server.url, "http") };
        if value.as_ref().is_none_or(|v| v.trim().is_empty()) {
            return Some(format!("mcpServers.{name}.{field}: Required for enabled {kind} server"));
        }
    }
    None
}
pub fn default_settings() -> McpSettings {
    McpSettings { tool_prefix: Some("mcp".into()), search_threshold: Some(10.0), output_guard: Some(OutputGuardSettings { max_bytes: Some(51200.0), max_lines: Some(2000.0), max_tokens: None }), ..Default::default() }
}
