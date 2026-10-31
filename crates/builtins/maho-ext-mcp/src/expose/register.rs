use super::schema_compat::{
    McpToolNameEntry, build_mcp_tool_names, convert_json_schema_to_type_box, map_mcp_tool_result,
};
use crate::{
    catalog::McpToolCatalogEntry,
    config_schema::OutputGuardSettings,
    guard::output_guard::{McpOutputArtifacts, McpOutputGuardOptions, apply_mcp_output_guard},
};
use maho_ext_api::{ToolContent, ToolDefinition, ToolError, ToolExecutionMode, ToolResult};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc};
#[derive(Clone)]
pub struct McpNamedCatalogEntry {
    pub entry: McpToolCatalogEntry,
    pub name: String,
}
pub fn map_mcp_catalog_names(entries: &[McpToolCatalogEntry]) -> Vec<McpNamedCatalogEntry> {
    let collator = icu_collator::Collator::try_new(Default::default(), Default::default())
        .expect("compiled collation data is available");
    let mut entries = entries.to_vec();
    entries.sort_by(|left, right| {
        collator
            .compare(&left.server, &right.server)
            .then_with(|| collator.compare(&left.tool, &right.tool))
    });
    let names = build_mcp_tool_names(
        &entries
            .iter()
            .map(|entry| McpToolNameEntry {
                server_name: entry.server.clone(),
                tool_name: entry.tool.clone(),
            })
            .collect::<Vec<_>>(),
        None,
    );
    entries
        .into_iter()
        .zip(names)
        .map(|(entry, name)| McpNamedCatalogEntry { entry, name })
        .collect()
}
pub fn build_mcp_tool_definitions(
    entries: &[McpToolCatalogEntry],
    agent_dir: PathBuf,
    artifacts: Arc<McpOutputArtifacts>,
    output_guard: Option<OutputGuardSettings>,
) -> Vec<ToolDefinition> {
    map_mcp_catalog_names(entries).into_iter().map(|named|{
        let entry=named.entry;let label=format!("{}/{}",entry.server,entry.tool);
        let description=entry.description.clone().unwrap_or_else(||format!("MCP tool {label}"));
        let schema=super::schema_compat::convert_json_schema_to_type_box_with(&entry.schema,entry.modern).schema;
        let agent_dir=agent_dir.clone();let artifacts=artifacts.clone();let output_guard=output_guard.clone();
        let mut tool=ToolDefinition::new(&named.name,&description,schema,Arc::new(move |call|{
            let entry=entry.clone();let agent_dir=agent_dir.clone();let artifacts=artifacts.clone();let output_guard=output_guard.clone();
            Box::pin(async move {
                call.signal.check()?;
                // invocation.ts: a modern tool's arguments validate against its exact 2020-12 schema; an
                // unusable schema refuses the call as schema_changed.
                if entry.modern {
                    let refusal=match crate::modern::tool_schema::compile_modern_schema(&entry.schema) {
                        Err(error)=>Some(format!("{}: {}",error.code.as_str(),error.message)),
                        Ok(validator) if !validator.validate(&call.params)=>Some("Arguments do not match the current MCP tool schema.".to_owned()),
                        Ok(_)=>None,
                    };
                    if let Some(message)=refusal {
                        return Ok(refused_result(&entry,"schema_changed",&message,entry.agent_dir.as_deref().unwrap_or(&agent_dir),entry.artifacts.as_ref().unwrap_or(&artifacts),entry.output_guard.as_ref().or(output_guard.as_ref())));
                    }
                }
                let params=if call.params.is_object() || entry.modern {call.params}else{json!({})};
                let result=super::call::call_mcp_tool(&entry,params,call.id,&call.signal,call.on_update.as_deref()).await?;
                mapped_guarded_result(&entry,&result,entry.agent_dir.as_deref().unwrap_or(&agent_dir),entry.artifacts.as_ref().unwrap_or(&artifacts),entry.output_guard.as_ref().or(output_guard.as_ref()))
            })
        }));
        tool.label=label;tool.execution_mode=Some(ToolExecutionMode::Parallel);tool
    }).collect()
}
/// register.ts `executeMcpCatalogEntry` refused branch: a refusal is a result the model can read, not a thrown
/// error; it carries `{ error: { kind, message, server, tool, currentDefinition } }`.
fn refused_result(
    entry: &McpToolCatalogEntry,
    kind: &str,
    message: &str,
    agent_dir: &std::path::Path,
    artifacts: &McpOutputArtifacts,
    output_guard: Option<&OutputGuardSettings>,
) -> ToolResult {
    let error = json!({"kind":kind,"message":message,"server":entry.server,"tool":entry.tool,"currentDefinition":entry.schema});
    let guarded = apply_mcp_output_guard(
        &[json!({"type":"text","text":json!({"error":error}).to_string()})],
        McpOutputGuardOptions {
            agent_dir,
            artifacts: Some(artifacts),
            server: &entry.server,
            output_guard,
        },
    );
    ToolResult {
        content: guarded
            .iter()
            .map(|block| ToolContent::text(block.get("text").and_then(Value::as_str).unwrap_or("")))
            .collect(),
        details: Some(maho_core::config_reference_redaction::redact_resolved_reference_data(
            &json!({"error":error,"preview":kind,"server":entry.server,"tool":entry.tool}),
        )),
        structured_content: None,
        is_error: None,
    }
}
pub fn mapped_guarded_result(
    entry: &McpToolCatalogEntry,
    result: &Value,
    agent_dir: &std::path::Path,
    artifacts: &McpOutputArtifacts,
    output_guard: Option<&OutputGuardSettings>,
) -> Result<ToolResult, ToolError> {
    let normalized = if result.get("content").is_some()
        || result.get("structuredContent").is_some()
        || result.get("isError").is_some()
    {
        result.clone()
    } else {
        json!({"structuredContent":result.get("toolResult").cloned().unwrap_or(Value::Null)})
    };
    let mapped = map_mcp_tool_result(&normalized);
    if mapped.get("ok") == Some(&Value::Bool(false)) {
        return Err(ToolError::Message(
            maho_core::config_reference_redaction::redact_resolved_reference_values(
                mapped["error"]["message"]
                    .as_str()
                    .unwrap_or("MCP tool returned an error result."),
                "<redacted>",
            ),
        ));
    }
    let blocks = mapped
        .get("content")
        .and_then(Value::as_array)
        .map_or(&[][..], Vec::as_slice);
    let guarded = apply_mcp_output_guard(
        blocks,
        McpOutputGuardOptions {
            agent_dir,
            artifacts: Some(artifacts),
            server: &entry.server,
            output_guard,
        },
    );
    let content = guarded
        .into_iter()
        .map(|block| match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                ToolContent::text(block.get("text").and_then(Value::as_str).unwrap_or(""))
            }
            Some("image") => ToolContent::Image {
                data: block["data"].as_str().unwrap_or("").into(),
                mime_type: block["mimeType"].as_str().unwrap_or("").into(),
            },
            _ => ToolContent::text(block.to_string()),
        })
        .collect::<Vec<_>>();
    let preview = content
        .iter()
        .map(|block| match block {
            ToolContent::Text { text, .. } => text.clone(),
            ToolContent::Image { mime_type, .. } => format!("[{mime_type} image]"),
        })
        .collect::<Vec<_>>()
        .join(" ");
    let preview = preview.trim();
    let preview = if preview.is_empty() {
        "(empty result)".into()
    } else if preview.encode_utf16().count() > 120 {
        format!(
            "{}...",
            String::from_utf16_lossy(&preview.encode_utf16().take(117).collect::<Vec<_>>())
        )
    } else {
        preview.to_owned()
    };
    Ok(ToolResult {
        content,
        details: Some(json!({"preview":preview,"server":entry.server,"tool":entry.tool})),
        structured_content: None,
        is_error: None,
    })
}

/// register.ts `truncatePreview`: at most 120 UTF-16 units, else the first 117 plus `...`.
fn truncate_preview(value: &str) -> String {
    let units = value.encode_utf16().collect::<Vec<_>>();
    if units.len() <= 120 {
        value.to_owned()
    } else {
        format!("{}...", String::from_utf16_lossy(&units[..117]))
    }
}

/// JavaScript `String.prototype.trim`: Unicode white space and the BOM, but not NEL (U+0085).
pub(crate) fn js_trim(value: &str) -> &str {
    value.trim_matches(|character: char| {
        (character.is_whitespace() && character != '\u{85}') || character == '\u{feff}'
    })
}

/// The text `details.preview ?? fallback` interpolates into a template literal.
pub(crate) fn details_preview(details: &Value, fallback: &str) -> String {
    match details.get("preview") {
        None | Some(Value::Null) => fallback.to_owned(),
        Some(Value::String(preview)) => preview.clone(),
        Some(other) => other.to_string(),
    }
}

/// register.ts `renderCall`/`renderResult` for a direct or searchable MCP tool:
/// the call is the bold `toolTitle` `name <args JSON preview>`, the result is the
/// `toolOutput` `name: running` while partial, else `name: <details.preview>`.
pub fn mcp_tool_renderers(name: &str) -> crate::tool_registrar::McpToolRenderers {
    use maho_tui::components::text::Text;
    let call_name = name.to_owned();
    let result_name = name.to_owned();
    maho_ext_api::ToolRenderers {
        render_call: Some(Arc::new(move |args, theme, _context| {
            let preview = truncate_preview(&args.to_string());
            let title = format!("{call_name} {preview}");
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
                    details_preview(&result.details, "(empty result)")
                )
            };
            Box::new(Text::with_padding(theme.fg("toolOutput", &title), 0, 0))
        })),
    }
}
