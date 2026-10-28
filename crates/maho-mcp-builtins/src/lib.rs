//! Builtin MCP servers for the MIT profile (`docs/cleanroom/specs/mcp.md`).
//!
//! The MCP client itself is `maho-ext-mcp` (MIT port of senpi's client: stdio and streamable HTTP
//! transports, `mcp.json` config, `mcp_<server>_<tool>` naming, enable/disable). This crate only
//! declares two public remote servers on it through `ExtensionApi::register_mcp_server`, so a
//! user or project `mcp.json` entry with the same name always wins.

use maho_config::Env;
use maho_ext_api::{
    Extension, ExtensionApi, McpAuth, McpExposure, McpLifecycle, McpServerDeclaration, McpTransport,
};
use serde_json::Value;
use std::path::Path;

/// One builtin server: its name and streamable HTTP endpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuiltinServer {
    pub name: &'static str,
    pub url: &'static str,
    /// Environment variable holding an optional bearer token.
    pub token_env: Option<&'static str>,
}

/// The builtin servers, in declaration order.
pub const BUILTIN_SERVERS: &[BuiltinServer] = &[
    BuiltinServer {
        name: "context7",
        url: "https://mcp.context7.com/mcp",
        token_env: Some("CONTEXT7_API_KEY"),
    },
    BuiltinServer {
        name: "grep_app",
        url: "https://mcp.grep.app",
        token_env: None,
    },
];

/// Environment switch: `0`, `false`, `off` or `no` declares no builtin server.
pub const SWITCH_ENV: &str = "MAHO_BUILTIN_MCPS";

/// Inputs to [`declarations`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BuiltinSettings {
    /// `MAHO_BUILTIN_MCPS` turned them off.
    pub switched_off: bool,
    /// Offline mode (`--offline`, `PI_OFFLINE`, `MAHO_OFFLINE`).
    pub offline: bool,
    /// `mcp.builtins` from the merged `omo.jsonc` view (default true).
    pub config_enabled: bool,
    /// `mcp.disabled_builtins`, union over layers.
    pub disabled: Vec<String>,
    /// Token variables that are set (non-empty) in the environment.
    pub tokens_present: Vec<String>,
}

impl Default for BuiltinSettings {
    fn default() -> Self {
        Self {
            switched_off: false,
            offline: false,
            config_enabled: true,
            disabled: Vec::new(),
            tokens_present: Vec::new(),
        }
    }
}

fn falsy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "off" | "no"
    )
}

fn truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl BuiltinSettings {
    /// Settings from an environment map and the `omo.jsonc` layers for `cwd`.
    pub fn resolve(cwd: &Path, env: &Env) -> Self {
        let get = |name: &str| env.get(name).map(String::as_str).unwrap_or_default();
        let loaded = maho_config::load_native(cwd, env);
        let config_enabled = loaded
            .view
            .get("mcp")
            .and_then(|mcp| mcp.get("builtins"))
            .and_then(Value::as_bool)
            .unwrap_or(true);
        Self {
            switched_off: falsy(get(SWITCH_ENV)),
            offline: truthy(get("PI_OFFLINE")) || truthy(get("MAHO_OFFLINE")),
            config_enabled,
            disabled: maho_config::collect_disabled_mcp_builtins(
                &loaded.layers,
                &maho_config::Harness::Native,
                loaded.profile.as_deref(),
            ),
            tokens_present: BUILTIN_SERVERS
                .iter()
                .filter_map(|server| server.token_env)
                .filter(|name| !get(name).trim().is_empty())
                .map(str::to_owned)
                .collect(),
        }
    }
}

/// The server declarations `settings` allows (mcp.md §2, §3).
pub fn declarations(settings: &BuiltinSettings) -> Vec<(&'static str, McpServerDeclaration)> {
    if settings.switched_off || settings.offline || !settings.config_enabled {
        return Vec::new();
    }
    BUILTIN_SERVERS
        .iter()
        .filter(|server| !settings.disabled.iter().any(|name| name == server.name))
        .map(|server| {
            let token = server
                .token_env
                .filter(|name| settings.tokens_present.iter().any(|set| set == name));
            let declaration = McpServerDeclaration {
                transport: Some(McpTransport::Http),
                url: Some(server.url.to_owned()),
                auth: Some(if token.is_some() {
                    McpAuth::Bearer
                } else {
                    McpAuth::Disabled
                }),
                bearer_token_env: token.map(str::to_owned),
                lifecycle: Some(McpLifecycle::Lazy),
                exposure: Some(McpExposure::Auto),
                ..Default::default()
            };
            (server.name, declaration)
        })
        .collect()
}

/// The `<builtin:mcp-builtins>` extension. Reads the process environment and the `omo.jsonc`
/// layers of the registration cwd when it registers.
#[derive(Clone, Copy, Debug, Default)]
pub struct McpBuiltinsExtension;

impl Extension for McpBuiltinsExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let settings = BuiltinSettings::resolve(&api.cwd, &maho_config::process_env());
        for (name, declaration) in declarations(&settings) {
            if let Err(error) = api.try_register_mcp_server(name, declaration) {
                maho_ext_api::diagnostic(&format!("mcp-builtins: {}", error.message));
            }
        }
    }
}
