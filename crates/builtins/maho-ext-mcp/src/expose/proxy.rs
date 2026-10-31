//! Proxy operations: describe/call via the native catalog executor plus the
//! Tier-C gateway tool, whose search/nearest ops use the shared BM25 index.
use maho_ext_api::{
    ToolCall, ToolContent, ToolDefinition, ToolError, ToolExecutionMode, ToolResult,
};
use maho_ext_tool_search::engine::bm25::{Bm25SearchOptions, build_bm25_index};
use maho_ext_tool_search::engine::document::{ToolSearchDocument, ToolSearchSource};
use maho_ext_tool_search::engine::marker::derive_mcp_registration_id;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

use super::register::{build_mcp_tool_definitions, map_mcp_catalog_names};

use crate::catalog::McpToolCatalogEntry;

fn text_result(server: &str, text: String) -> ToolResult {
    let preview = String::from_utf16_lossy(
        &text
            .split('\n')
            .next()
            .unwrap_or("")
            .encode_utf16()
            .take(80)
            .collect::<Vec<_>>(),
    );
    ToolResult {
        content: vec![ToolContent::text(text)],
        details: Some(json!({"preview": preview, "server": server, "tool": "proxy"})),
        structured_content: None,
        is_error: None,
    }
}

/// Describe a selected catalog entry without connecting to the server.
pub fn describe_mcp_proxy_entry(
    entry: &McpToolCatalogEntry,
) -> Result<ToolResult, serde_json::Error> {
    let empty_schema = json!({});
    Ok(text_result(
        &entry.server,
        format!(
            "{}: {}\nInput schema (JSON Schema):\n{}",
            entry.tool,
            entry.description.as_deref().unwrap_or("(no description)"),
            serde_json::to_string_pretty(if entry.schema.is_null() {
                &empty_schema
            } else {
                &entry.schema
            })?,
        ),
    ))
}

/// Parse proxy JSON-string arguments and invoke the existing native definition.
/// Signal, update callback and context are forwarded without schema validation,
/// matching the explicitly opted-in proxy contract.
pub async fn call_mcp_proxy_entry(
    entry: &McpToolCatalogEntry,
    full: &ToolDefinition,
    args: Option<&str>,
    mut call: ToolCall<'_>,
) -> Result<ToolResult, ToolError> {
    match parse_proxy_args(entry, args) {
        Ok(params) => {
            call.params = params;
            (full.execute)(call).await
        }
        Err(guidance) => Ok(guidance),
    }
}

/// The proxy's JSON-string `args` as an object, or the guiding text result.
fn parse_proxy_args(entry: &McpToolCatalogEntry, args: Option<&str>) -> Result<Value, ToolResult> {
    let parsed = serde_json::from_str::<Value>(args.unwrap_or("{}"));
    match parsed {
        Ok(params) if params.is_object() => Ok(params),
        result => {
            let error = match result {
                Ok(_) => "args must encode a JSON object".to_owned(),
                Err(error) => error.to_string(),
            };
            Err(text_result(
                &entry.server,
                format!(
                    "Invalid args for '{}': {error}. Pass args as a JSON object STRING matching the schema from op:\"describe\", e.g. {{\"op\":\"call\",\"tool\":\"{}\",\"args\":\"{{\\\"key\\\": \\\"value\\\"}}\"}}.",
                    entry.tool, entry.tool,
                ),
            ))
        }
    }
}

/// Tier-C gateway (todo 38): one always-active `mcp_<server>` tool per proxy
/// server, with the search -> describe -> call flow. Errors stay guiding text.
pub fn create_mcp_proxy_tool(
    server: &str,
    entries: &[McpToolCatalogEntry],
    agent_dir: std::path::PathBuf,
    artifacts: Arc<crate::guard::output_guard::McpOutputArtifacts>,
    output_guard: Option<crate::config_schema::OutputGuardSettings>,
) -> ToolDefinition {
    let named = map_mcp_catalog_names(entries);
    let by_tool: Arc<BTreeMap<String, (McpToolCatalogEntry, String)>> = Arc::new(
        named
            .iter()
            .map(|named| {
                (
                    named.entry.tool.clone(),
                    (named.entry.clone(), named.name.clone()),
                )
            })
            .collect(),
    );
    let documents: Vec<ToolSearchDocument> = named
        .iter()
        .map(|named| ToolSearchDocument {
            name: named.name.clone(),
            label: named.entry.tool.clone(),
            aliases: vec![named.entry.tool.clone()],
            description: named.entry.description.clone(),
            search_text: None,
            keywords: Vec::new(),
            source: ToolSearchSource::Mcp,
            group: server.into(),
            owner_label: server.into(),
            registration_id: derive_mcp_registration_id(server, &named.entry.tool),
        })
        .collect();
    let index = Arc::new(build_bm25_index(&documents));
    let defaults = super::register::McpGuardDefaults {
        agent_dir: agent_dir.clone(),
        artifacts: artifacts.clone(),
        output_guard: output_guard.clone(),
    };
    let full: Arc<BTreeMap<String, ToolDefinition>> = Arc::new(
        build_mcp_tool_definitions(entries, agent_dir, artifacts, output_guard)
            .into_iter()
            .map(|definition| (definition.name.clone(), definition))
            .collect(),
    );
    let name = named.first().map_or_else(
        || format!("mcp_{server}"),
        |named| named.name.split('_').take(2).collect::<Vec<_>>().join("_"),
    );
    let tool_count = entries.len();
    let description = format!(
        "Gateway to the '{server}' MCP server ({tool_count} tools). Use op:\"search\" with a query to find tools, op:\"describe\" with a tool name for its schema, then op:\"call\" with tool + args (args is a JSON object STRING: no strict validation, so match the described schema exactly)."
    );
    let server = server.to_owned();
    let closure_server = server.clone();
    let gateway_name = name.clone();
    let identity_by_tool = by_tool.clone();
    let mut definition = ToolDefinition::new(
        &name,
        &description,
        json!({"type":"object","properties":{"op":{"anyOf":[{"const":"search","type":"string"},{"const":"describe","type":"string"},{"const":"call","type":"string"}],"description":"search: rank tools by capability; describe: full schema for one tool; call: invoke a tool."},"query":{"type":"string","description":"search: capability description to rank tools by."},"tool":{"type":"string","description":"describe/call: the server-side tool name."},"args":{"type":"string","description":"call: tool arguments as a JSON object STRING."}},"required":["op"]}),
        Arc::new(move |call| {
            let server = closure_server.clone();
            let by_tool = by_tool.clone();
            let index = index.clone();
            let full = full.clone();
            let defaults = defaults.clone();
            let gateway_name = gateway_name.clone();
            Box::pin(async move {
                let op = call.params.get("op").and_then(Value::as_str).unwrap_or("");
                if op == "search" {
                    let query = call
                        .params
                        .get("query")
                        .and_then(Value::as_str)
                        .unwrap_or("");
                    let matches = index.search(
                        query,
                        10,
                        &Bm25SearchOptions {
                            source: None,
                            group: None,
                            exact_match: None,
                            precision: None,
                        },
                    );
                    let body = if matches.is_empty() {
                        format!(
                            "No '{server}' tools matched. Try broader keywords or op:\"describe\" with an exact tool name."
                        )
                    } else {
                        matches
                            .iter()
                            .map(|matched| {
                                format!(
                                    "- {} - {}",
                                    matched.doc.label,
                                    matched
                                        .doc
                                        .description
                                        .as_deref()
                                        .unwrap_or("(no description)")
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n")
                    };
                    return Ok(text_result(
                        &server,
                        format!("Tools on '{server}':\n{body}"),
                    ));
                }
                let requested = call.params.get("tool").and_then(Value::as_str);
                let Some((entry, mcp_name)) = requested
                    .and_then(|requested| by_tool.get(requested))
                    .cloned()
                else {
                    let nearest = requested.map_or_else(Vec::new, |requested| {
                        index.search(
                            requested,
                            3,
                            &Bm25SearchOptions {
                                source: None,
                                group: None,
                                exact_match: None,
                                precision: None,
                            },
                        )
                    });
                    let hint = if nearest.is_empty() {
                        String::new()
                    } else {
                        format!(
                            " Nearest matches: {}.",
                            nearest
                                .iter()
                                .map(|matched| matched.doc.label.clone())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    };
                    return Ok(text_result(
                        &server,
                        format!(
                            "Unknown tool '{}' on '{server}'.{hint} Use op:\"search\" to list tools.",
                            requested.unwrap_or("(missing)")
                        ),
                    ));
                };
                if op == "describe" {
                    return Ok(describe_mcp_proxy_entry(&entry).unwrap_or_else(|error| {
                        text_result(
                            &server,
                            format!("Failed to describe '{}': {error}", entry.tool),
                        )
                    }));
                }
                let args = call
                    .params
                    .get("args")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                if entry.invocation.is_some() {
                    // proxy.ts: the call is the gateway's invocation (its name and raw params), the
                    // operation its preflight approved.
                    let params = match parse_proxy_args(&entry, args.as_deref()) {
                        Ok(params) => params,
                        Err(guidance) => return Ok(guidance),
                    };
                    let invocation = crate::invocation::McpInvocation::from_call(
                        &call,
                        &gateway_name,
                        call.params.clone(),
                    );
                    return super::register::execute_mcp_catalog_entry(
                        &entry,
                        params,
                        &call,
                        &invocation,
                        &defaults,
                    )
                    .await;
                }
                let Some(full) = full.get(&mcp_name) else {
                    return Ok(text_result(
                        &server,
                        format!("Tool '{}' is not registered for proxy calls.", entry.tool),
                    ));
                };
                call_mcp_proxy_entry(&entry, full, args.as_deref(), call).await
            })
        }),
    );
    // proxy.ts `registerDispatchIdentity(parameters, ...)`: a `call` names the operation it dispatches.
    maho_ext_permission_system::dispatch_metadata::register_dispatch_identity(
        &mut definition,
        move |input| {
            if input.get("op").and_then(Value::as_str) != Some("call") {
                return None;
            }
            let (entry, _) = identity_by_tool.get(input.get("tool")?.as_str()?)?;
            Some(entry.invocation.as_ref()?.identity(entry))
        },
    );
    definition.label = format!("MCP proxy for {server}");
    definition.prompt_snippet = Some(format!(
        "Proxy for the {server} MCP server: search -> describe -> call (args as a JSON string)."
    ));
    definition.execution_mode = Some(ToolExecutionMode::Parallel);
    definition
}

/// A JSON argument as a JavaScript template literal interpolates it (`${value}`).
fn template_text(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Object(_)) => "[object Object]".into(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                item => template_text(Some(item)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(other) => other.to_string(),
    }
}

/// `args.<key> ?? ""` interpolated into a template literal.
fn template_or_empty(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        value => template_text(value),
    }
}

/// proxy.ts `renderCall`/`renderResult` for the Tier-C gateway: the call is the
/// bold `toolTitle` `name op <query|tool>`, the result is the `toolOutput`
/// `name: running` while partial, else `name: <details.preview ?? "done">`.
pub fn mcp_proxy_renderers(name: &str) -> crate::tool_registrar::McpToolRenderers {
    use super::register::{details_preview, js_trim};
    use maho_tui::components::text::Text;
    let call_name = name.to_owned();
    let result_name = name.to_owned();
    maho_ext_api::ToolRenderers {
        render_call: Some(Arc::new(move |args, theme, _context| {
            let op = args.get("op");
            let detail = if op.and_then(Value::as_str) == Some("search") {
                template_or_empty(args.get("query"))
            } else {
                template_or_empty(args.get("tool"))
            };
            let title = format!("{call_name} {} {detail}", template_text(op));
            Box::new(Text::with_padding(
                theme.fg("toolTitle", &theme.bold(js_trim(&title))),
                0,
                0,
            ))
        })),
        render_result: Some(Arc::new(move |result, options, theme, _context| {
            let title = if options.is_partial {
                format!("{result_name}: running")
            } else {
                format!(
                    "{result_name}: {}",
                    details_preview(&result.details, "done")
                )
            };
            Box::new(Text::with_padding(theme.fg("toolOutput", &title), 0, 0))
        })),
    }
}
