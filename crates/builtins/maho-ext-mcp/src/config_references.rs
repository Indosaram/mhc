//! Port of senpi mcp/config-references.ts: configuration references in an MCP server declaration.
//!
//! Only `headers.*`, `env.*` and `args[]` accept a reference object. A trusted file remembers its origin on each
//! reference as it is read; every other source refuses references before any I/O.
use crate::config_schema::{McpServerConfig, McpServerDeclaredConfig, ServerConfigWire};
use maho_ai::utils::config_reference::{
    ConfigReference, ConfigReferenceError, ConfigReferenceReason, parse_config_reference,
};
use maho_core::config_reference_resolver::{
    ConfigReferenceContext, ConfigValue, config_reference_json, remember_config_reference_origin,
};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerReference {
    pub field: String,
    pub reference: ConfigReference,
}

/// The accepting positions of one declared server: `headers.*`, `env.*` and `args[]`.
pub fn mcp_server_references(
    name: &str,
    server: &McpServerDeclaredConfig,
) -> Vec<McpServerReference> {
    // Upstream visits headers, then env, then args, each in declaration order.
    let order = |field: &str| match field.split('.').next() {
        Some("headers") => 0,
        Some("env") => 1,
        _ => 2,
    };
    let mut found = server
        .references
        .iter()
        .filter_map(|(field, value)| {
            value.reference().map(|reference| McpServerReference {
                field: format!("mcpServers.{name}.{field}"),
                reference: reference.clone(),
            })
        })
        .collect::<Vec<_>>();
    found.sort_by_key(|reference| {
        let field = reference
            .field
            .trim_start_matches(&format!("mcpServers.{name}."))
            .to_owned();
        let index = field
            .strip_prefix("args.")
            .and_then(|index| index.parse::<usize>().ok())
            .unwrap_or(0);
        (order(&field), index)
    });
    found
}

/// Called as a trusted file is read, so each reference resolves against the file that declares it.
pub fn remember_mcp_reference_origins(
    servers: Option<&mut BTreeMap<String, ServerConfigWire>>,
    origin: &str,
) {
    for (name, server) in servers.into_iter().flatten() {
        for (field, value) in &mut server.references {
            remember_config_reference_origin(
                value,
                ConfigReferenceContext::trusted(origin, format!("mcpServers.{name}.{field}")),
            );
        }
    }
}

/// A source that does not expand legacy `${NAME}` refuses every reference before any I/O.
pub fn refuse_mcp_references(
    name: &str,
    server: &McpServerDeclaredConfig,
) -> Result<(), ConfigReferenceError> {
    match mcp_server_references(name, server).into_iter().next() {
        Some(first) => Err(ConfigReferenceError::new(
            first.field,
            config_reference_json(&first.reference),
            ConfigReferenceReason::Forbidden,
        )),
        None => Ok(()),
    }
}

/// Refuses the first reference with its named error, then narrows the declaration to plain strings.
/// The placeholder slots a reference occupied are dropped, matching upstream's string filter.
pub fn plain_mcp_declaration(
    name: &str,
    server: &McpServerDeclaredConfig,
) -> Result<McpServerDeclaredConfig, ConfigReferenceError> {
    refuse_mcp_references(name, server)?;
    let mut plain = server.clone();
    plain.references.clear();
    Ok(plain)
}

/// The normalized form of a plain declaration, as a connection config (no reference left to resolve).
pub fn plain_mcp_server_config(
    name: &str,
    server: &McpServerDeclaredConfig,
) -> Result<McpServerConfig, ConfigReferenceError> {
    plain_mcp_declaration(name, server)
}

/// Splits the reference objects out of one raw server declaration's accepting positions, leaving an empty string
/// placeholder so the declaration deserializes as plain strings. A malformed object stays in place for the schema
/// check to reject.
pub fn extract_mcp_references(server: &mut Value) -> BTreeMap<String, ConfigValue> {
    let mut references = BTreeMap::new();
    let mut take = |field: String, slot: &mut Value| {
        if let Some(reference) = parse_config_reference(slot) {
            references.insert(
                field,
                ConfigValue::Reference {
                    reference,
                    origin: None,
                },
            );
            *slot = Value::String(String::new());
        }
    };
    if let Some(args) = server.get_mut("args").and_then(Value::as_array_mut) {
        for (index, slot) in args.iter_mut().enumerate() {
            take(format!("args.{index}"), slot);
        }
    }
    for map in ["headers", "env"] {
        if let Some(entries) = server.get_mut(map).and_then(Value::as_object_mut) {
            for (key, slot) in entries.iter_mut() {
                take(format!("{map}.{key}"), slot);
            }
        }
    }
    references
}

/// skill-server.ts `referenceRefusal`: where `${NAME}` stays literal (remote or untrusted skill servers and every
/// project skill server), a reference is refused before any I/O, as a warning naming the skipped server.
pub fn reference_refusal(name: &str, raw: &Value, label: &str) -> Option<String> {
    let mut probe = raw.clone();
    let references = extract_mcp_references(&mut probe);
    let first = references.into_iter().next()?;
    let error = ConfigReferenceError::new(
        format!("mcpServers.{name}.{}", first.0),
        first.1.to_json(),
        ConfigReferenceReason::Forbidden,
    );
    Some(format!("{label} skipped: {error}"))
}
