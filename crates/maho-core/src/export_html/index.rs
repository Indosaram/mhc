//! Self-contained HTML export for persisted agent sessions.

use std::path::{Path, PathBuf};
use serde_json::{json, Value};

const TEMPLATE: &str = include_str!("templates/template.html");
const CSS: &str = include_str!("templates/template.css");
const JS: &str = include_str!("templates/template.js");
const MARKED: &str = include_str!("templates/vendor/marked.min.js");
const HIGHLIGHT: &str = include_str!("templates/vendor/highlight.min.js");

fn app_name() -> &'static str { "maho" }

fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(path)
}

fn normalize_output(path: &str) -> PathBuf { expand_tilde(path) }

fn export_html(header: Value, entries: Vec<Value>, leaf_id: Option<String>, output: Option<&str>, theme_name: Option<&str>, system_prompt: Option<String>, tools: Option<Value>) -> Result<String, String> {
    let session = json!({"header": header, "entries": entries, "leafId": leaf_id, "systemPrompt": system_prompt, "tools": tools});
    let encoded = base64_encode(serde_json::to_vec(&session).map_err(|e| e.to_string())?.as_slice());
    let theme = theme_name.unwrap_or("default");
    let vars = format!("--themeName: {};\n      --exportPageBg: rgb(24, 24, 30);\n      --exportCardBg: rgb(30, 30, 36);\n      --exportInfoBg: rgb(60, 55, 40);", theme);
    let css = CSS.replace("{{THEME_VARS}}", &vars).replace("{{BODY_BG}}", "rgb(24, 24, 30)").replace("{{CONTAINER_BG}}", "rgb(30, 30, 36)").replace("{{INFO_BG}}", "rgb(60, 55, 40)");
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

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let a=chunk[0] as u32; let b=chunk.get(1).copied().unwrap_or(0) as u32; let c=chunk.get(2).copied().unwrap_or(0) as u32;
        let n=(a<<16)|(b<<8)|c; out.push(TABLE[((n>>18)&63) as usize] as char); out.push(TABLE[((n>>12)&63) as usize] as char);
        out.push(if chunk.len()>1 {TABLE[((n>>6)&63) as usize] as char} else {'='}); out.push(if chunk.len()>2 {TABLE[(n&63) as usize] as char} else {'='});
    } out
}

pub fn export_from_file(input_path: &str, output_path: Option<&str>, theme_name: Option<&str>) -> Result<String, String> {
    let input = Path::new(input_path);
    if !input.exists() { return Err(format!("File not found: {}", input.display())); }
    let path = input.to_string_lossy().into_owned();
    let manager = crate::session_manager::SessionManager::open(&path, None, None, None);
    export_html(manager.header().unwrap_or(Value::Null), manager.entries(), manager.leaf_id().map(str::to_owned), output_path, theme_name, None, None)
}

pub fn export_session_to_html(session: &crate::session_manager::SessionManager, output_path: Option<&str>, theme_name: Option<&str>, system_prompt: Option<String>) -> Result<String, String> {
    let file = session.session_file().ok_or_else(|| "Cannot export in-memory session to HTML".to_owned())?;
    if !Path::new(file).exists() { return Err("Nothing to export yet - start a conversation first".to_owned()); }
    export_html(session.header().unwrap_or(Value::Null), session.entries(), session.leaf_id().map(str::to_owned), output_path, theme_name, system_prompt, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn base64_round_trips_known_prefix() { assert_eq!(base64_encode(b"hello"), "aGVsbG8="); }
    #[test] fn tilde_path_expands_home() { assert!(expand_tilde("~/x").to_string_lossy().ends_with("/x")); }
    #[test] fn provider_native_template_has_collapsible_payload() { assert!(JS.contains("provider-native-block") && JS.contains("rawBody.length > 2000")); }
    #[test] fn provider_native_css_uses_theme_variables() { assert!(CSS.contains(".provider-native-block") && CSS.contains("color: var(--muted);")); }
    #[test] fn whitespace_css_preserves_output_lines() { assert!(CSS.contains("white-space: pre-wrap") && CSS.contains(".ansi-line")); }
    #[test] fn markdown_links_are_scheme_sanitized() { assert!(JS.contains("sanitizeMarkdownUrl(token.href)") && JS.contains("https?")); }
    #[test] fn markdown_images_are_scheme_sanitized() { assert!(JS.contains("image(token)") && JS.contains("sanitizeMarkdownUrl(token.href)")); }
    #[test] fn markdown_urls_strip_controls() { assert!(JS.contains("\\x00-\\x1f\\x7f")); }
    #[test] fn markdown_links_escape_href() { assert!(JS.contains("escapeHtml(href)")); }
    #[test] fn markdown_images_escape_attributes() { assert!(JS.contains("escapeHtml(img.mimeType") && JS.contains("escapeHtml(img.data")); }
    #[test] fn entry_ids_are_escaped() { assert!(JS.contains("entry-${escapeHtml(entry.id)}") && JS.contains("data-entry-id=\"${escapeHtml(entryId)}\"")); }
    #[test] fn tree_metadata_is_escaped() { assert!(JS.contains("escapeHtml(msg.toolName") && JS.contains("escapeHtml(entry.modelId)")); }
    #[test] fn model_names_are_escaped() { assert!(JS.contains("escapeHtml(globalStats.models.join")); }
    #[test] fn skill_blocks_render_as_siblings() { assert!(JS.contains("skill-invocation") && JS.contains("hasUserContent")); }
    #[test] fn skill_content_uses_markdown() { assert!(JS.contains("safeMarkedParse(skill.content)")); }
}


