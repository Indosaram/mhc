//! Port of senpi mcp/skill-server.ts (v2026.10.27-2).
//!
//! Skill-declared MCP servers (mcp.json sidecar or SKILL.md frontmatter) resolve with the trust of the skill that
//! declared them (senpi#2345). Stdio children see only the allowlisted environment plus `env`, so `${VAR}`
//! expansion decides which parent secrets a skill-chosen command can read:
//! - a user-owned skill, or a project skill of a trusted project, expands stdio values exactly like trusted
//!   mcp.json;
//! - an untrusted project's skill keeps them literal;
//! - a remote server from any skill keeps url/headers literal and never sends a bearerTokenEnv variable, because
//!   the skill chose where the request goes.
//!
//! Each kept-literal or dropped value produces one warning naming the fix.
use crate::{
    config::McpConfigValidationError,
    config_schema::{ResolvedMcpServer, ServerConfigWire},
};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

pub struct SkillServerSource<'a> {
    pub skill_name: &'a str,
    /// A user-owned skill, or a project skill of a trusted project.
    pub trusted: bool,
    pub env: &'a BTreeMap<String, String>,
}

#[derive(Debug, Default)]
pub struct ResolvedSkillServer {
    /// Absent when the declaration was rejected; the warning says why.
    pub server: Option<ResolvedMcpServer>,
    pub warning: Option<String>,
}

const OWN_CONFIG_FIX: &str = "declare the server in your own mcp.json instead";

/// Resolve a skill-declared MCP server. Exposure is forced to search with no directTools (see
/// `config::resolve_skill_mcp_server`) so the catalog registers with zero active tools until the owning skill
/// loads; lifecycle stays lazy. A declaration that does not parse fails like any other skill server.
pub fn resolve_skill_mcp_server(
    name: &str,
    raw: &Value,
    source_path: &Path,
    source: &SkillServerSource<'_>,
    agent_dir: Option<&Path>,
) -> Result<ResolvedSkillServer, McpConfigValidationError> {
    let label = format!("Skill '{}' MCP server '{name}'", source.skill_name);
    let (declaration, warnings) = declare_for_trust(name, raw, &label, source);
    let Some(declaration) = declaration else {
        return Ok(ResolvedSkillServer {
            server: None,
            warning: Some(warnings.join(" ")),
        });
    };
    let raw_wire = server_wire(raw.clone())?;
    let mut declared = server_wire(declaration)?;
    let mut servers = BTreeMap::from([(name.to_owned(), declared)]);
    crate::config_references::remember_mcp_reference_origins(
        Some(&mut servers),
        &source_path.to_string_lossy(),
    );
    declared = servers.remove(name).expect("inserted above");
    crate::sharing_policy::inherit_mcp_sharing_scope(&raw_wire, &mut declared);
    let server = crate::config::resolve_skill_mcp_server(name, declared, source_path, agent_dir)?;
    Ok(ResolvedSkillServer {
        server: Some(server),
        warning: (!warnings.is_empty()).then(|| warnings.join(" ")),
    })
}

fn server_wire(mut value: Value) -> Result<ServerConfigWire, McpConfigValidationError> {
    let references = crate::config_references::extract_mcp_references(&mut value);
    let mut wire: ServerConfigWire = serde_json::from_value(value)
        .map_err(|error| McpConfigValidationError(error.to_string()))?;
    wire.references = references;
    Ok(wire)
}

fn declare_for_trust(
    name: &str,
    raw: &Value,
    label: &str,
    source: &SkillServerSource<'_>,
) -> (Option<Value>, Vec<String>) {
    let remote = is_remote(raw);
    if (remote || !source.trusted)
        && let Some(refused) = reference_refusal(name, raw, label)
    {
        return (None, vec![refused]);
    }
    if remote {
        let (declaration, warnings) = declare_remote(raw, label);
        return (Some(declaration), warnings);
    }
    if !source.trusted {
        let mut literal = vec![raw.get("command"), raw.get("cwd")];
        literal.extend(
            raw.get("args")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(Some),
        );
        literal.extend(
            raw.get("env")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(|env| env.values())
                .map(Some),
        );
        let warning = literal_warning(
            label,
            literal.into_iter().flatten(),
            "stays literal because the skill comes from an untrusted project; trust the project or move the server into your own mcp.json to expand it.",
        );
        return (Some(raw.clone()), warning.into_iter().collect());
    }
    match crate::config::interpolate_value(raw, &format!("mcp.mcpServers.{name}"), source.env) {
        Ok(declaration) => (Some(declaration), Vec::new()),
        Err(error) => (None, vec![format!("{label} skipped: {}", error.0)]),
    }
}

/// `(raw.type ?? (raw.url ? "http" : "stdio")) !== "stdio"`.
fn is_remote(raw: &Value) -> bool {
    let kind = match raw.get("type") {
        Some(Value::String(kind)) => kind.as_str(),
        Some(Value::Null) | None => {
            if raw.get("url").is_some_and(truthy) {
                "http"
            } else {
                "stdio"
            }
        }
        // A non-string type is never "stdio".
        Some(_) => "",
    };
    kind != "stdio"
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::String(value) => !value.is_empty(),
        Value::Number(value) => value
            .as_f64()
            .is_some_and(|value| value != 0.0 && !value.is_nan()),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn declare_remote(raw: &Value, label: &str) -> (Value, Vec<String>) {
    let mut warnings = Vec::new();
    let mut literal = vec![raw.get("url")];
    literal.extend(
        raw.get("headers")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|headers| headers.values())
            .map(Some),
    );
    if let Some(warning) = literal_warning(
        label,
        literal.into_iter().flatten(),
        &format!(
            "in url/headers stays literal: a skill-declared remote server never expands environment variables, because the skill chooses where they are sent; {OWN_CONFIG_FIX}."
        ),
    ) {
        warnings.push(warning);
    }
    let auth = raw.get("auth").filter(|auth| !auth.is_null());
    let bearer_env = raw.get("bearerTokenEnv").filter(|env| !env.is_null());
    let sends_bearer =
        auth == Some(&Value::String("bearer".into())) || (auth.is_none() && bearer_env.is_some());
    let Some(bearer_env) = bearer_env.filter(|_| sends_bearer) else {
        return (raw.clone(), warnings);
    };
    let shown = bearer_env
        .as_str()
        .map_or_else(|| bearer_env.to_string(), str::to_owned);
    warnings.push(format!(
        "{label}: bearerTokenEnv '{shown}' is ignored and no Authorization header is sent, because a skill-declared remote server must not send a parent environment variable to a server the skill chose; {OWN_CONFIG_FIX}, where bearerTokenEnv keeps working."
    ));
    let mut declaration = raw.clone();
    if let Some(declaration) = declaration.as_object_mut() {
        declaration.remove("bearerTokenEnv");
        declaration.insert("auth".into(), Value::Bool(false));
    }
    (declaration, warnings)
}

/// Where `${NAME}` stays literal, a reference is refused before any I/O.
pub use crate::config_references::reference_refusal;

fn literal_warning<'a>(
    label: &str,
    values: impl IntoIterator<Item = &'a Value>,
    reason: &str,
) -> Option<String> {
    let pattern = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)").expect("constant regex");
    let mut names: Vec<String> = Vec::new();
    for value in values {
        let Some(value) = value.as_str() else {
            continue;
        };
        for capture in pattern.captures_iter(value) {
            let name = format!("${{{}}}", &capture[1]);
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    (!names.is_empty()).then(|| format!("{label}: {} {reason}", names.join(", ")))
}
