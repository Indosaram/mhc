//! Port of senpi `packages/coding-agent/src/modes/rpc/project-mcp-commands.ts`.
//!
//! `get_project_mcp_servers` / `set_project_mcp_servers`: read and edit the session folder's
//! project MCP file over RPC.
//!
//! `maho-rpc` cannot depend on `maho-ext-mcp` (would cycle through `maho-interactive`), so
//! [`set_project_mcp_servers`] accepts an optional injected [`McpConfigValidator`] callback and
//! falls back to [`structural_mcp_config_validator`] when none is supplied.

use crate::project_file_path::{
    PROJECT_FILE_MAX_BYTES, ProjectFilePath, ResolvedProjectFile, resolve_project_file,
};
use crate::project_file_store::{Stored, etag_of, read_stored, serialize_write};
use crate::project_write_gate::{
    GuardedWrite, ProjectWriteRefusal, WriteAuthorizer, commit_guarded_write, conflict,
    not_allowed, not_trusted, project_trusted_for_write, too_large,
};
use crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT;
use maho_core::agent_session::AgentSession;
use maho_core::config::config_dir_name;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub const RPC_ERROR_PROJECT_MCP_CONFIG_UNPARSEABLE: &str = "project_mcp_config_unparseable";
pub const RPC_ERROR_PROJECT_MCP_CONFIG_INVALID: &str = "project_mcp_config_invalid";

/// Injected MCP config validator (mirrors `assertLoadableMcpConfig` in `maho-ext-mcp`).
pub type McpConfigValidator = dyn Fn(&Value, &Path) -> Result<(), String> + Send + Sync;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcProjectMcpServers {
    pub cwd: String,
    /// The folder-relative path of the project MCP file.
    pub path: String,
    pub exists: bool,
    /// The servers as stored (placeholders unexpanded); `None` when the file is missing or does not parse.
    pub mcp_servers: Option<Map<String, Value>>,
    pub parse_error: Option<String>,
    /// Opaque version of the stored bytes; compare it, never parse it.
    pub etag: Option<String>,
    pub project_trusted: bool,
}

pub type ProjectMcpResult = Result<RpcProjectMcpServers, ProjectWriteRefusal>;

struct Parsed {
    raw: Option<Map<String, Value>>,
    parse_error: Option<String>,
}

struct McpFileState {
    data: RpcProjectMcpServers,
    raw: Option<Map<String, Value>>,
    absolute: PathBuf,
    mode: Option<u32>,
    missing_folders: Vec<PathBuf>,
}

fn mcp_file_path() -> ProjectFilePath {
    let dir = config_dir_name();
    ProjectFilePath {
        relative: format!("{dir}/mcp.json"),
        segments: vec![dir, "mcp.json".to_owned()],
    }
}

fn parse_mcp_file(bytes: &[u8]) -> Parsed {
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => {
            return Parsed {
                raw: None,
                parse_error: Some(err.to_string()),
            };
        }
    };
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(err) => {
            return Parsed {
                raw: None,
                parse_error: Some(err.to_string()),
            };
        }
    };
    let Some(obj) = value.as_object() else {
        return Parsed {
            raw: None,
            parse_error: Some("The file is not a JSON object.".to_owned()),
        };
    };
    if let Some(servers) = obj.get("mcpServers")
        && !servers.is_object()
    {
        return Parsed {
            raw: None,
            parse_error: Some("'mcpServers' is not a JSON object.".to_owned()),
        };
    }
    Parsed {
        raw: Some(obj.clone()),
        parse_error: None,
    }
}

fn stored_servers(raw: &Map<String, Value>) -> Map<String, Value> {
    raw.get("mcpServers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Structural JSON fallback validator when no `maho-ext-mcp` validator callback is provided.
///
/// Enforces that `candidate` is an object, `mcpServers` is an object, and every server entry is an
/// object containing a valid stdio (`command: string`) or remote (`url: string`) configuration.
pub fn structural_mcp_config_validator(candidate: &Value, _path: &Path) -> Result<(), String> {
    let Some(root) = candidate.as_object() else {
        return Err("MCP config must be a JSON object.".to_owned());
    };
    let Some(servers) = root.get("mcpServers").and_then(Value::as_object) else {
        return Err("MCP config 'mcpServers' must be a JSON object.".to_owned());
    };
    for (name, entry) in servers {
        if name.trim().is_empty() {
            return Err("MCP server name must not be empty.".to_owned());
        }
        let Some(obj) = entry.as_object() else {
            return Err(format!("MCP server '{name}' must be a JSON object."));
        };
        let has_command = obj
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        let has_url = obj
            .get("url")
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty());
        if !has_command && !has_url {
            return Err(format!(
                "MCP server '{name}' must define a non-empty 'command' or 'url'."
            ));
        }
        if let Some(args) = obj.get("args") {
            let Some(arr) = args.as_array() else {
                return Err(format!("MCP server '{name}' 'args' must be an array."));
            };
            if !arr.iter().all(Value::is_string) {
                return Err(format!(
                    "MCP server '{name}' 'args' must contain only strings."
                ));
            }
        }
        for map_key in ["env", "headers"] {
            if let Some(field) = obj.get(map_key) {
                let Some(map) = field.as_object() else {
                    return Err(format!(
                        "MCP server '{name}' '{map_key}' must be an object."
                    ));
                };
                if !map.values().all(Value::is_string) {
                    return Err(format!(
                        "MCP server '{name}' '{map_key}' values must be strings."
                    ));
                }
            }
        }
    }
    Ok(())
}

async fn read_mcp_state(
    session: &AgentSession,
    mcp_file: &ProjectFilePath,
) -> Result<McpFileState, ProjectWriteRefusal> {
    let cwd = session.cwd();
    let project_trusted = session.with_settings_manager(|s| s.is_project_trusted());
    let resolved = resolve_project_file(Path::new(&cwd), mcp_file)
        .await
        .map_err(|_| not_allowed(&mcp_file.relative))?;
    let ResolvedProjectFile::Ok {
        absolute,
        exists,
        missing_folders,
    } = resolved
    else {
        return Err(not_allowed(&mcp_file.relative));
    };
    let stored = if exists {
        read_stored(&absolute)
            .await
            .map_err(|_| not_allowed(&mcp_file.relative))?
    } else {
        Stored::Missing
    };
    match stored {
        Stored::Refused => Err(not_allowed(&mcp_file.relative)),
        Stored::TooLarge => Err(too_large()),
        Stored::Missing => Ok(McpFileState {
            data: RpcProjectMcpServers {
                cwd,
                path: mcp_file.relative.clone(),
                exists: false,
                mcp_servers: None,
                parse_error: None,
                etag: None,
                project_trusted,
            },
            raw: None,
            absolute,
            mode: None,
            missing_folders,
        }),
        Stored::File { bytes, mode } => {
            let parsed = parse_mcp_file(&bytes);
            let mcp_servers = parsed.raw.as_ref().map(stored_servers);
            Ok(McpFileState {
                data: RpcProjectMcpServers {
                    cwd,
                    path: mcp_file.relative.clone(),
                    exists: true,
                    mcp_servers,
                    parse_error: parsed.parse_error,
                    etag: Some(etag_of(&bytes)),
                    project_trusted,
                },
                raw: parsed.raw,
                absolute,
                mode: Some(mode),
                missing_folders,
            })
        }
    }
}

pub async fn get_project_mcp_servers(session: &AgentSession) -> ProjectMcpResult {
    let mcp_file = mcp_file_path();
    let state = read_mcp_state(session, &mcp_file).await?;
    Ok(state.data)
}

fn merge_servers(current: &Map<String, Value>, patch: &Map<String, Value>) -> Map<String, Value> {
    let mut merged = Map::new();
    for (name, entry) in current {
        if !patch.contains_key(name) {
            merged.insert(name.clone(), entry.clone());
        }
    }
    for (name, entry) in patch {
        if !entry.is_null() {
            merged.insert(name.clone(), entry.clone());
        }
    }
    merged
}

async fn set_checked(
    session: &AgentSession,
    mcp_file: &ProjectFilePath,
    patch: &Map<String, Value>,
    if_etag: Option<&str>,
    validator: Option<&McpConfigValidator>,
    authorizer: Option<&WriteAuthorizer>,
) -> ProjectMcpResult {
    let state = read_mcp_state(session, mcp_file).await?;
    if state.data.etag.as_deref() != if_etag {
        return Err(conflict(&mcp_file.relative, state.data.etag.as_deref()));
    }
    if let Some(parse_error) = &state.data.parse_error {
        return Err(ProjectWriteRefusal {
            message: format!(
                "{} does not parse ({parse_error}); fix it before editing servers.",
                mcp_file.relative
            ),
            error_code: RPC_ERROR_PROJECT_MCP_CONFIG_UNPARSEABLE.to_owned(),
            error_data: Some(json!({ "parseError": parse_error })),
        });
    }
    let mut candidate = state.raw.clone().unwrap_or_default();
    let merged = merge_servers(&stored_servers(&candidate), patch);
    candidate.insert("mcpServers".to_owned(), Value::Object(merged));
    let candidate_value = Value::Object(candidate);
    let check = validator.unwrap_or(&structural_mcp_config_validator);
    if let Err(message) = check(&candidate_value, &state.absolute) {
        return Err(ProjectWriteRefusal {
            message,
            error_code: RPC_ERROR_PROJECT_MCP_CONFIG_INVALID.to_owned(),
            error_data: None,
        });
    }
    let mut content =
        serde_json::to_string_pretty(&candidate_value).map_err(|err| ProjectWriteRefusal {
            message: err.to_string(),
            error_code: RPC_ERROR_PROJECT_MCP_CONFIG_INVALID.to_owned(),
            error_data: None,
        })?;
    content.push('\n');
    if content.len() > PROJECT_FILE_MAX_BYTES {
        return Err(too_large());
    }
    if let Some(refusal) = commit_guarded_write(
        session,
        GuardedWrite {
            relative: mcp_file.relative.clone(),
            absolute: state.absolute,
            content,
            expected_etag: state.data.etag,
            mode: state.mode,
            missing_folders: state.missing_folders,
        },
        authorizer,
    )
    .await
    {
        return Err(refusal);
    }
    get_project_mcp_servers(session).await
}

pub async fn set_project_mcp_servers(
    session: &AgentSession,
    patch: Option<&Value>,
    if_etag: Option<&Value>,
    validator: Option<&McpConfigValidator>,
    authorizer: Option<&WriteAuthorizer>,
) -> ProjectMcpResult {
    let Some(patch_obj) = patch.and_then(Value::as_object) else {
        return Err(ProjectWriteRefusal {
            message: "set_project_mcp_servers requires an object 'patch' (server name to entry, or null to remove) and 'ifEtag' (the etag read, or null to create).".to_owned(),
            error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
            error_data: None,
        });
    };
    let parsed_etag = match if_etag {
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.as_str()),
        _ => {
            return Err(ProjectWriteRefusal {
                message: "set_project_mcp_servers requires an object 'patch' (server name to entry, or null to remove) and 'ifEtag' (the etag read, or null to create).".to_owned(),
                error_code: RPC_ERROR_INVALID_ARGUMENT.to_owned(),
                error_data: None,
            });
        }
    };
    if !project_trusted_for_write(session).await {
        return Err(not_trusted());
    }
    let mcp_file = mcp_file_path();
    let key = Path::new(&session.cwd()).join(&mcp_file.relative);
    serialize_write(key, || {
        set_checked(
            session,
            &mcp_file,
            patch_obj,
            parsed_etag,
            validator,
            authorizer,
        )
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_merge_servers_preserve_siblings_and_delete_nulls() {
        let parsed = parse_mcp_file(
            br#"{"mcpServers":{"a":{"command":"npx"},"b":{"url":"https://x"}},"extra":1}"#,
        );
        assert!(parsed.parse_error.is_none());
        let raw = parsed.raw.expect("raw");
        let patch = serde_json::from_value::<Map<String, Value>>(
            json!({"a": null, "c": {"command": "uvx"}}),
        )
        .expect("patch");
        let merged = merge_servers(&stored_servers(&raw), &patch);
        assert!(!merged.contains_key("a"));
        assert_eq!(merged.get("b"), Some(&json!({"url": "https://x"})));
        assert_eq!(merged.get("c"), Some(&json!({"command": "uvx"})));
    }

    #[test]
    fn structural_validator_rejects_invalid_shapes() {
        assert!(
            structural_mcp_config_validator(
                &json!({"mcpServers":{"s":{"command":"node","args":["a.js"],"env":{"K":"V"}}}}),
                Path::new("/tmp/mcp.json")
            )
            .is_ok()
        );
        assert!(
            structural_mcp_config_validator(
                &json!({"mcpServers":{"s":{}}}),
                Path::new("/tmp/mcp.json")
            )
            .is_err()
        );
        assert!(
            structural_mcp_config_validator(
                &json!({"mcpServers":{"s":{"command":"node","args":[1]}}}),
                Path::new("/tmp/mcp.json")
            )
            .is_err()
        );
    }
}
