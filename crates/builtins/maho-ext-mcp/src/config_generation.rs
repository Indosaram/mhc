//! Port of senpi mcp/config-generation.ts: references resolved per connection generation.
use crate::{
    config_references::mcp_server_references,
    config_schema::{McpServerConfig, McpServerDeclaredConfig, ResolvedMcpServer},
    errors::{McpError, McpErrorKind},
};
use maho_ai::utils::config_reference::ConfigReferenceError;
use maho_core::config_reference_resolver::resolve_config_reference;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug, Clone)]
pub struct McpConfigGeneration {
    pub config: McpServerConfig,
    pub config_hash: String,
}

#[derive(Debug, Clone)]
pub struct McpGenerationScope {
    pub server_name: String,
    pub declared: McpServerDeclaredConfig,
    pub env: Option<BTreeMap<String, String>>,
    pub agent_dir: Option<PathBuf>,
}

/// A renewal whose snapshot no longer matches the admitted identity opens no transport; its owners re-key.
pub fn mcp_config_generation_changed_error(server_name: &str) -> McpError {
    let mut error = McpError::new(
        McpErrorKind::Connect,
        format!(
            "MCP server {server_name} configuration changed; it reconnects under its new identity"
        ),
    );
    error.phase = Some("generation".into());
    error.server_name = Some(server_name.into());
    error.cause = Some(Box::new(
        serde_json::json!({"name":"McpConfigGenerationChangedError"}),
    ));
    error
}

pub fn is_mcp_config_generation_changed_error(error: &McpError) -> bool {
    error.phase.as_deref() == Some("generation")
        && error
            .cause
            .as_deref()
            .and_then(|cause| cause.get("name"))
            .and_then(serde_json::Value::as_str)
            == Some("McpConfigGenerationChangedError")
}

/// One connection generation: every reference resolved into an immutable snapshot, then keyed by it.
pub fn resolve_mcp_config_generation(
    scope: &McpGenerationScope,
) -> Result<McpConfigGeneration, ConfigReferenceError> {
    let env = scope.env.as_ref().map(|env| {
        env.iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<HashMap<_, _>>()
    });
    let mut config = scope.declared.clone();
    let references = std::mem::take(&mut config.references);
    for (field, value) in &references {
        let resolved = resolve_config_reference(value, None, env.as_ref())?;
        let mut parts = field.splitn(2, '.');
        match (parts.next(), parts.next()) {
            (Some("args"), Some(index)) => {
                if let Some(slot) = index
                    .parse::<usize>()
                    .ok()
                    .and_then(|index| config.args.as_mut()?.get_mut(index))
                {
                    *slot = resolved;
                }
            }
            (Some("env"), Some(key)) => {
                config
                    .env
                    .get_or_insert_with(BTreeMap::new)
                    .insert(key.into(), resolved);
            }
            (Some("headers"), Some(key)) => {
                config
                    .headers
                    .get_or_insert_with(BTreeMap::new)
                    .insert(key.into(), resolved);
            }
            _ => {}
        }
    }
    crate::sharing_policy::inherit_mcp_sharing_scope(&scope.declared, &mut config);
    let config_hash =
        crate::config::hash_config(&config, scope.agent_dir.as_deref()).map_err(|error| {
            ConfigReferenceError::new(
                &scope.server_name,
                serde_json::Value::String(error.0),
                maho_ai::utils::config_reference::ConfigReferenceReason::Malformed,
            )
        })?;
    Ok(McpConfigGeneration {
        config,
        config_hash,
    })
}

/// For a server that declares references: every transport re-reads them, and a changed snapshot re-keys.
#[derive(Clone)]
pub struct McpGenerationRenewal {
    scope: McpGenerationScope,
    admitted_hash: String,
    rekey: Arc<dyn Fn() + Send + Sync>,
}
impl McpGenerationRenewal {
    pub fn resolve_config(&self) -> Result<McpServerConfig, McpError> {
        let next = resolve_mcp_config_generation(&self.scope)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))?;
        if next.config_hash == self.admitted_hash {
            return Ok(next.config);
        }
        (self.rekey)();
        Err(mcp_config_generation_changed_error(&self.scope.server_name))
    }
    pub fn changed(&self) -> Result<bool, McpError> {
        resolve_mcp_config_generation(&self.scope)
            .map(|next| next.config_hash != self.admitted_hash)
            .map_err(|error| McpError::new(McpErrorKind::Connect, error.to_string()))
    }
}
impl std::fmt::Debug for McpGenerationRenewal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpGenerationRenewal")
            .field("server", &self.scope.server_name)
            .finish_non_exhaustive()
    }
}

pub fn mcp_generation_renewal(
    scope: McpGenerationScope,
    admitted_hash: &str,
    rekey: Arc<dyn Fn() + Send + Sync>,
) -> Option<McpGenerationRenewal> {
    if mcp_server_references(&scope.server_name, &scope.declared).is_empty() {
        return None;
    }
    Some(McpGenerationRenewal {
        scope,
        admitted_hash: admitted_hash.into(),
        rekey,
    })
}

/// Resolves every wanted server's generation. A server whose reference fails is recorded in `failures` and
/// removed from `wanted`, so it never connects and never aborts its siblings.
pub fn resolve_wanted_mcp_generations(
    wanted: &mut BTreeMap<String, ResolvedMcpServer>,
    env: Option<&BTreeMap<String, String>>,
    agent_dir: Option<&Path>,
    log_dir: Option<&Path>,
    failures: &mut BTreeMap<String, ConfigReferenceError>,
) -> BTreeMap<String, McpConfigGeneration> {
    let mut generations = BTreeMap::new();
    failures.clear();
    let mut failed = Vec::new();
    for (name, server) in wanted.iter() {
        let Some(declared) = &server.config else {
            continue;
        };
        let scope = McpGenerationScope {
            server_name: name.clone(),
            declared: declared.clone(),
            env: env.cloned(),
            agent_dir: agent_dir.map(Path::to_path_buf),
        };
        match resolve_mcp_config_generation(&scope) {
            Ok(generation) => {
                // A server that declares no reference keeps its declared identity: re-hashing it under another
                // session's agent dir would re-key and respawn an unchanged server.
                let generation = match &server.config_hash {
                    Some(declared_hash) if mcp_server_references(name, declared).is_empty() => {
                        McpConfigGeneration {
                            config: generation.config,
                            config_hash: declared_hash.clone(),
                        }
                    }
                    _ => generation,
                };
                generations.insert(name.clone(), generation);
            }
            Err(error) => {
                if let Some(log_dir) = log_dir
                    && let Ok(mut logger) = crate::log::McpLogger::new(name, log_dir, None)
                {
                    let _ = logger.log(
                        "error",
                        ConfigReferenceError::NAME,
                        Some(&serde_json::json!({"field":error.field,"reference":error.reference,"reason":error.reason})),
                        None,
                    );
                }
                failures.insert(name.clone(), error);
                failed.push(name.clone());
            }
        }
    }
    for name in failed {
        wanted.remove(&name);
    }
    generations
}

/// A connection's identity: the declaration hash plus the resolved generation hash when resolution changed
/// anything. A server without references keeps its plain hash, so existing keys and cached catalogs stay valid.
pub fn mcp_connection_identity(declared: &str, generation: Option<&str>) -> String {
    match generation {
        Some(generation) if generation != declared => format!("{declared}\0{generation}"),
        _ => declared.to_owned(),
    }
}
