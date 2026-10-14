//! Self-contained HTML export for persisted agent sessions.

use std::path::{Path, PathBuf};
use base64::Engine as _;
use serde_json::{json, Value};

const TEMPLATE: &str = include_str!("templates/template.html");
const CSS: &str = include_str!("templates/template.css");
const JS: &str = include_str!("templates/template.js");
const MARKED: &str = include_str!("templates/vendor/marked.min.js");
const HIGHLIGHT: &str = include_str!("templates/vendor/highlight.min.js");

fn app_name() -> String { crate::config::app_name() }

fn parse_color(color: &str) -> Option<(u8, u8, u8)> {
    if let Some(hex) = color.strip_prefix('#') && hex.len() == 6 {
        return Some((u8::from_str_radix(&hex[0..2], 16).ok()?, u8::from_str_radix(&hex[2..4], 16).ok()?, u8::from_str_radix(&hex[4..6], 16).ok()?));
    }
    let values = color.strip_prefix("rgb(")?.strip_suffix(')')?;
    let mut values = values.split(',').map(str::trim).map(str::parse::<u8>);
    Some((values.next()?.ok()?, values.next()?.ok()?, values.next()?.ok()?))
}

fn relative_luminance((r, g, b): (u8, u8, u8)) -> f64 {
    let linear = |value: u8| { let v = f64::from(value) / 255.0; if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) } };
    0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b)
}

fn derive_export_colors(base: &str) -> (String, String, String) {
    let Some((r, g, b)) = parse_color(base) else {
        return ("rgb(24, 24, 30)".into(), "rgb(30, 30, 36)".into(), "rgb(60, 55, 40)".into());
    };
    let adjust = |factor: f64| format!("rgb({}, {}, {})", (f64::from(r) * factor).round().clamp(0.0, 255.0) as u8, (f64::from(g) * factor).round().clamp(0.0, 255.0) as u8, (f64::from(b) * factor).round().clamp(0.0, 255.0) as u8);
    let (page, card, info) = if relative_luminance((r,g,b)) > 0.5 {
        (adjust(0.96), base.to_owned(), format!("rgb({}, {}, {})", r.saturating_add(10), g.saturating_add(5), b.saturating_sub(20)))
    } else {
        (adjust(0.7), adjust(0.85), format!("rgb({}, {}, {})", r.saturating_add(20), g.saturating_add(15), b))
    };
    (page, card, info)
}

/// senpi `expandTildePath`: `normalizePath` with the default options.
fn expand_tilde(path: &str) -> PathBuf {
    PathBuf::from(crate::paths::normalize_path(path, &crate::paths::PathInputOptions::default()))
}

/// senpi: `normalizePath(outputPath)` followed by `expandTildePath`.
fn normalize_output(path: &str) -> PathBuf { expand_tilde(&crate::paths::normalize_path(path, &crate::paths::PathInputOptions::default())) }

type ExportPresentation = (Option<String>, Option<Value>, Option<Value>);
pub type ExportColors = (std::collections::BTreeMap<String, String>, std::collections::BTreeMap<String, String>);

fn export_html_data(header: Value, entries: Vec<Value>, leaf_id: Option<String>, output: Option<&str>, _theme_name: Option<&str>, (system_prompt, tools, rendered_tools): ExportPresentation, colors: ThemeColors) -> Result<String, String> {
    let session = json!({"header": header, "entries": entries, "leafId": leaf_id, "systemPrompt": system_prompt, "tools": tools, "renderedTools": rendered_tools});
    let encoded = base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&session).map_err(|e| e.to_string())?);
    let theme_colors = colors;
    let user_message_bg = theme_colors.colors.get("userMessageBg").map(String::as_str).unwrap_or("#343541");
    let (derived_page, derived_card, derived_info) = derive_export_colors(user_message_bg);
    let page = theme_colors.export.get("pageBg").unwrap_or(&derived_page);
    let card = theme_colors.export.get("cardBg").unwrap_or(&derived_card);
    let info = theme_colors.export.get("infoBg").unwrap_or(&derived_info);
    let mut vars = theme_colors.colors.iter().map(|(key, value)| format!("--{key}: {value};")).collect::<Vec<_>>();
    vars.push(format!("--exportPageBg: {page};"));
    vars.push(format!("--exportCardBg: {card};"));
    vars.push(format!("--exportInfoBg: {info};"));
    let vars = vars.join("\n      ");
    let css = CSS.replace("{{THEME_VARS}}", &vars).replace("{{BODY_BG}}", page).replace("{{CONTAINER_BG}}", card).replace("{{INFO_BG}}", info);
    let html = TEMPLATE.replace("{{CSS}}", &css).replace("{{JS}}", JS).replace("{{SESSION_DATA}}", &encoded).replace("{{MARKED_JS}}", MARKED).replace("{{HIGHLIGHT_JS}}", HIGHLIGHT);
    let output_path = match output {
        Some(path) => normalize_output(path),
        None => {
            let stem = session.get("header").and_then(|h| h.get("id")).and_then(Value::as_str).unwrap_or("session");
            PathBuf::from(format!("{}-session-{}.html", app_name(), stem))
        }
    };
    std::fs::write(&output_path, html).map_err(|e| e.to_string())?;
    Ok(output_path.to_string_lossy().into_owned())
}

#[derive(Default)]
struct ThemeColors {
    colors: std::collections::BTreeMap<String, String>,
    export: std::collections::BTreeMap<String, String>,
}

pub fn export_session_data_with_theme_to_html(header: Value, entries: Vec<Value>, leaf_id: Option<String>, output_path: Option<&str>, theme_name: Option<&str>, presentation: ExportPresentation, (colors, export_colors): ExportColors) -> Result<String, String> {
    export_html_data(header, entries, leaf_id, output_path, theme_name, presentation, ThemeColors { colors, export: export_colors })
}

pub fn export_session_to_html_with_theme(session: &crate::session_manager::SessionManager, output_path: Option<&str>, theme_name: Option<&str>, presentation: ExportPresentation, (colors, export_colors): ExportColors) -> Result<String, String> {
    let file = session.session_file().ok_or_else(|| "Cannot export in-memory session to HTML".to_owned())?;
    if !Path::new(file).exists() { return Err("Nothing to export yet - start a conversation first".to_owned()); }
    let output = output_path.map(str::to_owned).unwrap_or_else(|| format!("{}-session-{}.html", app_name(), Path::new(file).file_stem().map_or_else(|| "session".into(), |stem| stem.to_string_lossy().into_owned())));
    export_html_data(session.header().unwrap_or(Value::Null), session.entries(), session.leaf_id().map(str::to_owned), Some(&output), theme_name, presentation, ThemeColors { colors, export: export_colors })
}

fn export_html(header: Value, entries: Vec<Value>, leaf_id: Option<String>, output: Option<&str>, theme_name: Option<&str>, system_prompt: Option<String>, tools: Option<Value>) -> Result<String, String> {
    export_html_data(header, entries, leaf_id, output, theme_name, (system_prompt, tools, None), ThemeColors::default())
}

const TEMPLATE_RENDERED_TOOLS: [&str; 5] = ["bash", "read", "write", "edit", "ls"];

pub fn pre_render_custom_tools(entries: &[Value], renderer: &mut super::tool_renderer::ToolHtmlRenderer) -> Value {
    let mut rendered = serde_json::Map::new();
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("message") { continue; }
        let Some(message) = entry.get("message") else { continue; };
        match message.get("role").and_then(Value::as_str) {
            Some("assistant") => {
                let Some(content) = message.get("content").and_then(Value::as_array) else { continue; };
                for block in content {
                    if block.get("type").and_then(Value::as_str) != Some("toolCall") { continue; }
                    let Some(id) = block.get("id").and_then(Value::as_str) else { continue; };
                    let Some(name) = block.get("name").and_then(Value::as_str) else { continue; };
                    if TEMPLATE_RENDERED_TOOLS.contains(&name) { continue; }
                    if let Some(call_html) = renderer.render_call(id, name, block.get("arguments").cloned().unwrap_or(Value::Null)) {
                        rendered.entry(id.to_owned()).or_insert_with(|| json!({}))["callHtml"] = Value::String(call_html);
                    }
                }
            }
            Some("toolResult") => {
                let Some(id) = message.get("toolCallId").and_then(Value::as_str) else { continue; };
                let name = message.get("toolName").and_then(Value::as_str).unwrap_or_default();
                if !rendered.contains_key(id) && TEMPLATE_RENDERED_TOOLS.contains(&name) { continue; }
                let result = serde_json::from_value::<maho_agent::types::AgentToolResult>(message.clone());
                let Ok(result) = result else { continue; };
                if let Some(tool_html) = renderer.render_result(id, name, &result, result.is_error.unwrap_or(false)) {
                    let item = rendered.entry(id.to_owned()).or_insert_with(|| json!({}));
                    if let Some(collapsed) = tool_html.collapsed { item["resultHtmlCollapsed"] = Value::String(collapsed); }
                    item["resultHtmlExpanded"] = Value::String(tool_html.expanded);
                }
            }
            _ => {}
        }
    }
    Value::Object(rendered)
}

/// Export serialized session data, optionally including HTML pre-rendered by native tool renderers.
/// `rendered_tools` is keyed by tool-call id and uses `callHtml`, `resultHtmlCollapsed`, and `resultHtmlExpanded` fields.
pub fn export_session_data_to_html(header: Value, entries: Vec<Value>, leaf_id: Option<String>, output_path: Option<&str>, theme_name: Option<&str>, presentation: ExportPresentation) -> Result<String, String> {
    export_html_data(header, entries, leaf_id, output_path, theme_name, presentation, ThemeColors::default())
}

/// senpi `exportFromFile`: `colors` carries the resolved theme colors (`getResolvedThemeColors` /
/// `getThemeExportColors`); `None` falls back to the colors derived from the default background.
pub fn export_from_file(input_path: &str, output_path: Option<&str>, theme_name: Option<&str>, colors: Option<ExportColors>) -> Result<String, String> {
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let resolved = crate::paths::resolve_path(input_path, &cwd.to_string_lossy(), &crate::paths::PathInputOptions::default());
    if !Path::new(&resolved).exists() { return Err(format!("File not found: {resolved}")); }
    let manager = crate::session_manager::SessionManager::open(&resolved, None, None, None)?;
    let output = match output_path {
        Some(path) => path.to_owned(),
        None => format!("{}-session-{}.html", app_name(), Path::new(&resolved).file_stem().map_or_else(|| "session".into(), |stem| stem.to_string_lossy().into_owned())),
    };
    let colors = colors.map_or_else(ThemeColors::default, |(colors, export)| ThemeColors { colors, export });
    export_html_data(manager.header().unwrap_or(Value::Null), manager.entries(), manager.leaf_id().map(str::to_owned), Some(&output), theme_name, (None, None, None), colors)
}

pub fn export_session_to_html(session: &crate::session_manager::SessionManager, output_path: Option<&str>, theme_name: Option<&str>, system_prompt: Option<String>) -> Result<String, String> {
    export_session_to_html_with_tools(session, output_path, theme_name, system_prompt, None)
}

/// Pinned `exportSessionToHtml(sm, state, options)`: `tools` carries the agent state's tool metadata.
pub fn export_session_to_html_with_tools(session: &crate::session_manager::SessionManager, output_path: Option<&str>, theme_name: Option<&str>, system_prompt: Option<String>, tools: Option<Value>) -> Result<String, String> {
    let file = session.session_file().ok_or_else(|| "Cannot export in-memory session to HTML".to_owned())?;
    if !Path::new(file).exists() { return Err("Nothing to export yet - start a conversation first".to_owned()); }
    let output = match output_path {
        Some(path) => path.to_owned(),
        None => format!("{}-session-{}.html", app_name(), Path::new(file).file_stem().map_or_else(|| "session".into(), |stem| stem.to_string_lossy().into_owned())),
    };
    export_html(session.header().unwrap_or(Value::Null), session.entries(), session.leaf_id().map(str::to_owned), Some(&output), theme_name, system_prompt, tools)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn tilde_path_expands_home() { assert!(expand_tilde("~/x").to_string_lossy().ends_with("/x")); }

    #[test]
    fn export_from_file_reports_the_resolved_missing_path() {
        let error = export_from_file("definitely-missing-session.jsonl", None, None, None).expect_err("missing file");
        let expected = std::env::current_dir().expect("cwd").join("definitely-missing-session.jsonl");
        assert_eq!(error, format!("File not found: {}", expected.display()));
    }

    #[test]
    fn export_from_file_writes_themed_html_for_a_session_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let sessions = tmp.path().join("sessions");
        let mut manager = crate::session_manager::SessionManager::create(&cwd, Some(&sessions.to_string_lossy()), None);
        manager.append_message(json!({"role":"user","content":"hello","timestamp":1}));
        manager.append_message(json!({"role":"assistant","content":[{"type":"text","text":"world"}],"stopReason":"stop","timestamp":2}));
        let input = manager.session_file().expect("session file").to_owned();
        let output = tmp.path().join("from-file.html");
        let colors: std::collections::BTreeMap<String, String> = [("text".to_owned(), "#101010".to_owned())].into();
        let written = export_from_file(&input, output.to_str(), None, Some((colors, Default::default()))).expect("export from file");
        assert_eq!(written, output.to_string_lossy());
        assert!(std::fs::read_to_string(output).expect("read export").contains("--text: #101010;"));
    }
    #[test] fn session_payload_is_base64_embedded_and_carries_custom_tool_html() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let output = tmp.path().join("export.html");
        let rendered = json!({"call-1":{"callHtml":"<b>tool</b>","resultHtmlExpanded":"<i>result</i>"}});
        export_session_data_to_html(json!({"id":"session-1"}), vec![], None, output.to_str(), None, (None, None, Some(rendered))).expect("export HTML");
        let html = std::fs::read_to_string(output).expect("read HTML");
        assert!(!html.contains("{{"));
        assert!(html.contains("<style>") && html.contains("<script>"));
        let start = html.find("id=\"session-data\"").expect("session data element");
        let body = &html[start..];
        let encoded = &body[body.find('>').expect("element body") + 1..body.find("</script>").expect("element end")];
        let payload: Value = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(encoded.trim()).expect("base64 payload")).expect("session json");
        assert_eq!(payload["header"]["id"], "session-1");
        assert_eq!(payload["renderedTools"]["call-1"]["callHtml"], "<b>tool</b>");
    }

    #[test]
    fn theme_export_values_override_derived_colors_in_written_document() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let output = tmp.path().join("theme.html");
        let colors: std::collections::BTreeMap<String, String> = [("userMessageBg".to_owned(), "#ffffff".to_owned()), ("text".to_owned(), "#101010".to_owned())].into();
        let export: std::collections::BTreeMap<String, String> = [("pageBg".to_owned(), "#123456".to_owned()), ("cardBg".to_owned(), "#234567".to_owned()), ("infoBg".to_owned(), "#345678".to_owned())].into();
        let cwd = tmp.path().to_string_lossy().into_owned();
        let sessions = tmp.path().join("sessions");
        let mut manager = crate::session_manager::SessionManager::create(&cwd, Some(&sessions.to_string_lossy()), None);
        manager.append_message(json!({"role":"user","content":"hello","timestamp":1}));
        manager.append_message(json!({"role":"assistant","content":[{"type":"text","text":"world"}],"stopReason":"stop","timestamp":2}));
        export_session_to_html_with_theme(&manager, output.to_str(), Some("sample"), (None, None, Some(json!({"custom":{"callHtml":"<b>call</b>"}}))), (colors, export)).expect("export themed document");
        let html = std::fs::read_to_string(output).expect("read themed export");
        assert!(html.contains("--text: #101010;"));
        assert!(html.contains("--exportPageBg: #123456;") && html.contains("--exportCardBg: #234567;") && html.contains("--exportInfoBg: #345678;"));
        assert!(html.contains("--body-bg: #123456;") && html.contains("--container-bg: #234567;") && html.contains("--info-bg: #345678;"));
        let start = html.find("id=\"session-data\"").expect("session data element");
        let data = &html[start..];
        let encoded = &data[data.find('>').expect("element body") + 1..data.find("</script>").expect("element end")];
        let payload: Value = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(encoded.trim()).expect("decode exported session")).expect("parse exported session");
        assert_eq!(payload["systemPrompt"], Value::Null);
        assert_eq!(payload["renderedTools"]["custom"]["callHtml"], "<b>call</b>");
        assert!(payload["entries"].as_array().is_some_and(|entries| entries.len() == 2));
    }

    #[test]
    fn pre_render_walk_runs_custom_call_then_corresponding_result() {
        use std::sync::Arc;
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let lookup: super::super::tool_renderer::ToolRendererLookup = Arc::new(move |name| {
            if name != "custom" { return None; }
            Some(Arc::new(RecordedRenderer(recorded.clone())) as Arc<dyn maho_ext_api::ErasedToolRenderers>)
        });
        let mut renderer = super::super::tool_renderer::ToolHtmlRenderer::new(super::super::tool_renderer::ToolHtmlRendererDeps {
            get_tool_renderer: lookup,
            theme: maho_ext_api::Theme::default(),
            cwd: PathBuf::from("/work"),
            width: None,
        });
        let entries = vec![
            json!({"type":"message","id":"a","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"custom","arguments":{"x":1}}]}}),
            json!({"type":"message","id":"b","message":{"role":"toolResult","toolCallId":"call-1","toolName":"custom","content":[{"type":"text","text":"ok"}],"details":{},"isError":false}}),
        ];
        let rendered = pre_render_custom_tools(&entries, &mut renderer);
        assert_eq!(rendered["call-1"]["callHtml"], "<div class=\"ansi-line\">call</div>");
        assert_eq!(rendered["call-1"]["resultHtmlExpanded"], "<div class=\"ansi-line\">expanded</div>");
        assert_eq!(*calls.lock().expect("recorded calls"), vec!["call", "collapsed", "expanded"]);
    }

    #[test]
    fn pre_render_walk_renders_custom_result_stored_without_details() {
        use std::sync::Arc;
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let lookup: super::super::tool_renderer::ToolRendererLookup = Arc::new(move |_| {
            Some(Arc::new(RecordedRenderer(recorded.clone())) as Arc<dyn maho_ext_api::ErasedToolRenderers>)
        });
        let mut renderer = super::super::tool_renderer::ToolHtmlRenderer::new(super::super::tool_renderer::ToolHtmlRendererDeps {
            get_tool_renderer: lookup,
            theme: maho_ext_api::Theme::default(),
            cwd: PathBuf::from("/work"),
            width: None,
        });
        let entries = vec![
            json!({"type":"message","message":{"role":"toolResult","toolCallId":"call-1","toolName":"custom","content":[{"type":"text","text":"ok"}],"isError":false}}),
        ];
        let rendered = pre_render_custom_tools(&entries, &mut renderer);
        assert_eq!(rendered["call-1"]["resultHtmlExpanded"], "<div class=\"ansi-line\">expanded</div>");
    }

    #[test]
    fn pre_render_walk_leaves_template_rendered_tools_to_html_template() {
        use std::sync::Arc;
        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = calls.clone();
        let lookup: super::super::tool_renderer::ToolRendererLookup = Arc::new(move |_| {
            Some(Arc::new(RecordedRenderer(recorded.clone())) as Arc<dyn maho_ext_api::ErasedToolRenderers>)
        });
        let mut renderer = super::super::tool_renderer::ToolHtmlRenderer::new(super::super::tool_renderer::ToolHtmlRendererDeps {
            get_tool_renderer: lookup,
            theme: maho_ext_api::Theme::default(),
            cwd: PathBuf::from("/work"),
            width: None,
        });
        let entries = vec![
            json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"call-1","name":"read","arguments":{"path":"file"}}]}}),
            json!({"type":"message","message":{"role":"toolResult","toolCallId":"call-1","toolName":"read","content":[{"type":"text","text":"contents"}],"details":{},"isError":false}}),
        ];

        assert_eq!(pre_render_custom_tools(&entries, &mut renderer), json!({}));
        assert!(calls.lock().expect("recorded calls").is_empty());
    }

    struct RecordedRenderer(std::sync::Arc<std::sync::Mutex<Vec<&'static str>>>);
    impl maho_ext_api::ErasedToolRenderers for RecordedRenderer {
        fn render_call(&self, _: &Value, _: &maho_ext_api::Theme, _: usize) -> Option<Vec<String>> {
            self.0.lock().expect("calls").push("call");
            Some(vec!["call".into()])
        }
        fn render_result(&self, _: &Value, _: &maho_agent::types::AgentToolResult, _: &maho_ext_api::Theme, _: usize) -> Option<Vec<String>> {
            let mut calls = self.0.lock().expect("calls");
            let phase = if calls.contains(&"collapsed") { "expanded" } else { "collapsed" };
            calls.push(phase);
            Some(vec![phase.into()])
        }
    }
    #[test]
    fn persisted_session_export_embeds_json_base64_and_is_fully_self_contained() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cwd = tmp.path().to_string_lossy().into_owned();
        let sessions = tmp.path().join("sessions");
        let mut manager = crate::session_manager::SessionManager::create(&cwd, Some(&sessions.to_string_lossy()), None);
        let payload = "</script><script>alert('pwned')</script>";
        manager.append_message(json!({"role":"user","content":payload,"timestamp":1}));
        manager.append_message(json!({"role":"assistant","content":[{"type":"text","text":"done"}],"api":"faux","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":2}));
        let output = tmp.path().join("out.html");
        let tools = json!([{"name":"read","description":"Read a file","parameters":{"type":"object"}}]);
        export_session_to_html_with_tools(&manager, output.to_str(), None, Some("system".into()), Some(tools.clone())).expect("export");
        let html = std::fs::read_to_string(&output).expect("read HTML");
        assert!(!html.contains(payload), "session text must not reach the HTML unescaped");
        assert!(!html.contains("src=\"http") && !html.contains("href=\"http"), "export must not reference remote resources");
        let start = html.find("id=\"session-data\"").expect("session data element");
        let body = &html[start..];
        let encoded = &body[body.find('>').expect("element body") + 1..body.find("</script>").expect("element end")];
        let decoded: Value = serde_json::from_slice(&base64::engine::general_purpose::STANDARD.decode(encoded.trim()).expect("base64 session")).expect("session json");
        assert_eq!(decoded["tools"], tools);
        assert_eq!(decoded["systemPrompt"], "system");
        assert!(decoded["entries"].to_string().contains("pwned"));
    }

}


