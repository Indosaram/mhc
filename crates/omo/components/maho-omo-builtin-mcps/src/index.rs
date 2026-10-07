//! Port of `omo-senpi/src/components/builtin-mcps/index.ts` at latest
//! `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! Registers the two remote MCP servers the OpenCode edition injects at runtime so the native
//! edition reaches the same documentation and code-search surfaces. Both are used in well under 1%
//! of sessions while their schemas cost ~1.7K prompt tokens per request, so search exposure keeps
//! them out of the resident tool list until called by name.
//!
//! The key stays in the environment: senpi reads `bearerTokenEnv` at connect time, so a literal
//! Authorization header would only copy the secret into config dumps, diagnostics, and logs.

use std::collections::BTreeMap;

use maho_ext_api::{
    Extension, ExtensionApi, McpAuth, McpExposure, McpLifecycle, McpServerDeclaration, McpTransport,
};

pub const BUILTIN_MCPS_COMPONENT_NAME: &str = "builtin-mcps";
pub const CONTEXT7_SERVER_NAME: &str = "context7";
pub const CONTEXT7_URL: &str = "https://mcp.context7.com/mcp";
pub const CONTEXT7_API_KEY_ENV: &str = "CONTEXT7_API_KEY";
pub const GREP_APP_SERVER_NAME: &str = "grep_app";
pub const GREP_APP_URL: &str = "https://mcp.grep.app";
pub const DEFERRED_EXPOSURE: McpExposure = McpExposure::Search;

#[derive(Clone, Debug, Default)]
pub struct BuiltinMcpsComponentOptions {
    /// Upstream `options.env ?? process.env`.
    pub env: Option<BTreeMap<String, String>>,
}

pub struct BuiltinMcpsComponent {
    pub options: BuiltinMcpsComponentOptions,
}

impl BuiltinMcpsComponent {
    pub const NAME: &'static str = BUILTIN_MCPS_COMPONENT_NAME;

    pub fn new(options: BuiltinMcpsComponentOptions) -> Self {
        Self { options }
    }

    pub fn from_env(env: &BTreeMap<String, String>) -> Self {
        Self { options: BuiltinMcpsComponentOptions { env: Some(env.clone()) } }
    }
}

impl Extension for BuiltinMcpsComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let env = self.options.env.clone().unwrap_or_else(process_env);
        for (name, declaration) in builtin_mcp_declarations(&env) {
            api.register_mcp_server(name, declaration);
        }
    }
}

/// Upstream `register(pi)`: `registerMcpServer(context7)` then `registerMcpServer(grep_app)`.
pub fn builtin_mcp_declarations(env: &BTreeMap<String, String>) -> Vec<(&'static str, McpServerDeclaration)> {
    vec![
        (CONTEXT7_SERVER_NAME, create_context7_declaration(env)),
        (GREP_APP_SERVER_NAME, grep_app_declaration()),
    ]
}

pub fn create_context7_declaration(env: &BTreeMap<String, String>) -> McpServerDeclaration {
    let authenticated = has_context7_api_key(env.get(CONTEXT7_API_KEY_ENV).map(String::as_str));
    McpServerDeclaration {
        transport: Some(McpTransport::Http),
        url: Some(CONTEXT7_URL.to_owned()),
        enabled: Some(true),
        auth: Some(if authenticated { McpAuth::Bearer } else { McpAuth::Disabled }),
        bearer_token_env: authenticated.then(|| CONTEXT7_API_KEY_ENV.to_owned()),
        lifecycle: Some(McpLifecycle::Lazy),
        exposure: Some(DEFERRED_EXPOSURE),
        ..Default::default()
    }
}

pub fn grep_app_declaration() -> McpServerDeclaration {
    McpServerDeclaration {
        transport: Some(McpTransport::Http),
        url: Some(GREP_APP_URL.to_owned()),
        enabled: Some(true),
        auth: Some(McpAuth::Disabled),
        lifecycle: Some(McpLifecycle::Lazy),
        exposure: Some(DEFERRED_EXPOSURE),
        ..Default::default()
    }
}

/// Mirrors the OpenCode edition's placeholder normalization: copied `.env` templates carry
/// `<YOUR_API_KEY>`-style values, and sending one as a bearer token turns the anonymous-but-working
/// server into an authentication failure.
pub fn has_context7_api_key(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    let stripped: String = value
        .trim()
        .to_lowercase()
        .chars()
        .filter(|character| !matches!(character, '<' | '>' | '"' | '\'' | '`'))
        .collect();
    let mut normalized = String::with_capacity(stripped.len());
    let mut pending_separator = false;
    for character in stripped.chars() {
        if character.is_whitespace() || character == '_' || character == '-' {
            pending_separator = true;
            continue;
        }
        if pending_separator {
            normalized.push(' ');
            pending_separator = false;
        }
        normalized.push(character);
    }
    // Upstream's `replace(/[\s_-]+/g, " ")` also rewrites a trailing run into a space, which keeps
    // `"your api key-"` a *real* key rather than the placeholder.
    if pending_separator {
        normalized.push(' ');
    }
    !normalized.is_empty() && normalized != "your api key"
}

fn process_env() -> BTreeMap<String, String> {
    std::env::vars().collect()
}
