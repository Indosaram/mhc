//! Port of `packages/coding-agent/src/extensions/html-render/tool.ts` (senpi#5045, #6273, #6921).

use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use maho_agent::types::AgentToolResult;
use maho_ai::types::{ContentBlock, TextContent};
use maho_core::dynamic_prompt::{PromptSurface, resolve_prompt_surface};
use maho_ext_api::{
    ExtensionContext, ExtensionFailure, ExtensionToolExecutor, ToolDefinition, ToolError,
};
use serde_json::{Value, json};

use super::bootstrap::inject_html_render_bootstrap;
use super::handed_pages::{
    IMAGES_INLINED, MAX_HANDED_PAGE_CHARACTERS, PendingHandedPages, SHOW_HTML_PAGE_NAME,
    handed_page_text, over_session_budget_text, reserve_handed_page,
};
use super::images::inline_local_images;

pub const MAX_HTML_CHARACTERS: usize = 512_000;
const OUTPUT_DIR: &str = ".senpi/html-pages";

pub fn params_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "html": {
                "type": "string",
                "description": format!("A complete, self-contained HTML document, at most {MAX_HTML_CHARACTERS} characters. Pass screenshots and other images as absolute file paths (<img src=\"/abs/path.png\">), never as base64."),
                "minLength": 1
            },
            "title": {
                "type": "string",
                "description": "Short name for the page.",
                "minLength": 1,
                "maxLength": 200
            },
            "height": {
                "type": "integer",
                "description": "Frame height hint in CSS pixels, 80-2000. Defaults to the page's natural height."
            }
        },
        "required": ["html", "title"]
    })
}

pub const SHOW_HTML_PAGE_DESCRIPTION: &str = "Show a finished self-contained HTML page (chart, table, diagram, mockup) to the reader. Load the visualize skill before building the page. No network: inline every script, style and image (data: URIs). A <script src>, stylesheet link, font, image or fetch that points at a URL (a CDN included) is refused, and the page comes out blank or broken. Local images written as absolute file paths are inlined automatically after a magic-byte check (a renamed non-image is refused); pass screenshots that way, never as base64 data: URIs, since the page is limited to 512000 characters before images are inlined. In the OmO desktop thread the page renders inline above your reply; elsewhere it is only saved to a file and not displayed. The result says which happened.";

fn fail(message: impl Into<String>) -> ExtensionFailure {
    ExtensionFailure::new(message.into())
}

fn assert_within_input_limit(html: &str) -> Result<(), ExtensionFailure> {
    static BASE64_IMAGE: OnceLock<regex::Regex> = OnceLock::new();
    let length = html.encode_utf16().count();
    if length <= MAX_HTML_CHARACTERS {
        return Ok(());
    }
    let pattern = BASE64_IMAGE.get_or_init(|| {
        regex::Regex::new(r"(?i)data:image/[a-z0-9.+-]+;base64,[a-z0-9+/=\s]+")
            .unwrap_or_else(|error| panic!("invalid static pattern: {error}"))
    });
    let images: Vec<usize> = pattern
        .find_iter(html)
        .map(|found| found.as_str().encode_utf16().count())
        .collect();
    let advice = if images.is_empty() {
        " Shorten the document, or move large images out of it as absolute file paths, which show_html_page inlines itself.".to_owned()
    } else {
        format!(
            " {} base64 images make up {} of them. Save each image to a file and reference it by absolute file path (<img src=\"/abs/path/shot.png\">); show_html_page inlines local images itself, and they do not count against this limit.",
            images.len(),
            images.iter().sum::<usize>()
        )
    };
    Err(fail(format!(
        "The page is {length} characters; show_html_page accepts at most {MAX_HTML_CHARACTERS} characters.{advice}"
    )))
}

fn clamp_height(height: Option<f64>) -> Option<i64> {
    height.map(|h| (h.round() as i64).clamp(80, 2000))
}

fn safe_name(title: &str) -> String {
    static UNSAFE: OnceLock<regex::Regex> = OnceLock::new();
    static SPACES: OnceLock<regex::Regex> = OnceLock::new();
    let unsafe_chars = UNSAFE.get_or_init(|| {
        regex::Regex::new(r#"[\\/:*?"<>|\p{Cc}]+"#)
            .unwrap_or_else(|error| panic!("invalid static pattern: {error}"))
    });
    let spaces = SPACES.get_or_init(|| {
        regex::Regex::new(r"\s+").unwrap_or_else(|error| panic!("invalid static pattern: {error}"))
    });
    let cleaned = unsafe_chars.replace_all(title, " ");
    let collapsed = spaces.replace_all(&cleaned, " ");
    let name: String = collapsed.trim().chars().take(60).collect();
    if name.is_empty() {
        "page".to_owned()
    } else {
        name
    }
}

fn base36(mut value: u128) -> String {
    const DIGITS: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    if value == 0 {
        return "0".into();
    }
    let mut out = Vec::new();
    while value > 0 {
        out.push(DIGITS[(value % 36) as usize]);
        value /= 36;
    }
    out.reverse();
    String::from_utf8_lossy(&out).into_owned()
}

fn host_renders_page() -> bool {
    resolve_prompt_surface(|key| std::env::var(key).ok()) == PromptSurface::App
}

pub async fn write_html_page(
    html: &str,
    title: &str,
    height: Option<f64>,
    cwd: &std::path::Path,
    shown_inline: bool,
) -> Result<AgentToolResult, ExtensionFailure> {
    assert_within_input_limit(html)?;
    let inlined = inline_local_images(html)
        .await
        .map_err(|error| fail(error.to_string()))?;
    if !inlined.missing.is_empty() {
        return Err(fail(
            super::images::HtmlRenderImagesError::NotFound {
                paths: inlined.missing,
            }
            .to_string(),
        ));
    }
    let prepared = inject_html_render_bootstrap(&inlined.html);
    let height = clamp_height(height);
    let dir = cwd.join(OUTPUT_DIR);
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|error| fail(error.to_string()))?;
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |time| time.as_millis());
    let path = dir.join(format!("{}-{}.html", safe_name(title), base36(millis)));
    tokio::fs::write(&path, prepared)
        .await
        .map_err(|error| fail(error.to_string()))?;
    let path = path.to_string_lossy().into_owned();
    let inlined_chars = inlined.html.encode_utf16().count();
    let images_inlined = inlined_chars <= MAX_HANDED_PAGE_CHARACTERS;
    let result_text = handed_page_text(
        &path,
        shown_inline,
        images_inlined,
        images_inlined,
        0,
        inlined_chars,
    );
    Ok(AgentToolResult {
        content: vec![ContentBlock::Text(TextContent {
            text: result_text.clone(),
            audience: None,
            text_signature: None,
        })],
        details: json!({
            "path": path,
            "title": title,
            "height": height,
            "prepared": true,
            "html": if images_inlined { inlined.html } else { html.to_owned() },
            "imagesInlined": images_inlined,
            "resultText": result_text,
        }),
        structured_content: None,
        usage: None,
        nested_calls: None,
        added_tool_names: None,
        terminate: None,
        is_error: None,
    })
}

fn hand_html_page(
    tool_call_id: &str,
    written_html: &str,
    ctx: &ExtensionContext,
    pending: &Arc<Mutex<PendingHandedPages>>,
    mut result: AgentToolResult,
) -> AgentToolResult {
    let Some(obj) = result.details.as_object().cloned() else {
        return result;
    };
    if obj.get("imagesInlined").and_then(Value::as_bool) != Some(true) {
        return result;
    }
    let inlined_chars = obj
        .get("html")
        .and_then(Value::as_str)
        .map_or(0, |h| h.encode_utf16().count());
    let entries = ctx.session_manager.get_entries();
    let reservation = {
        let mut guard = pending.lock().unwrap_or_else(PoisonError::into_inner);
        reserve_handed_page(&entries, &mut guard, tool_call_id, inlined_chars)
    };
    if reservation.inlined {
        return result;
    }
    let over_budget = over_session_budget_text(reservation.already_handed);
    for block in &mut result.content {
        if let ContentBlock::Text(t) = block {
            t.text = t.text.replace(IMAGES_INLINED, &over_budget);
        }
    }
    let mut next = obj;
    next.insert("html".to_owned(), Value::String(written_html.to_owned()));
    next.insert("imagesInlined".to_owned(), Value::Bool(false));
    if let Some(Value::String(existing_result_text)) = next.get("resultText").cloned() {
        next.insert(
            "resultText".to_owned(),
            Value::String(existing_result_text.replace(IMAGES_INLINED, &over_budget)),
        );
    }
    result.details = Value::Object(next);
    result
}

pub fn hand_routed_html_page(
    tool_call_id: &str,
    params: &Value,
    ctx: &ExtensionContext,
    pending: &Arc<Mutex<PendingHandedPages>>,
    result: AgentToolResult,
) -> AgentToolResult {
    let Some(written_html) = params.get("html").and_then(Value::as_str) else {
        return result;
    };
    let Some(obj) = result.details.as_object() else {
        return result;
    };
    let valid_details = obj.get("path").and_then(Value::as_str).is_some()
        && obj.get("title").and_then(Value::as_str).is_some()
        && obj.get("prepared").and_then(Value::as_bool).is_some()
        && obj.get("html").and_then(Value::as_str).is_some()
        && obj.get("imagesInlined").and_then(Value::as_bool).is_some();
    if !valid_details {
        return result;
    }
    hand_html_page(tool_call_id, written_html, ctx, pending, result)
}

pub fn show_html_page_tool_definition() -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        SHOW_HTML_PAGE_NAME,
        SHOW_HTML_PAGE_DESCRIPTION,
        params_schema(),
        Arc::new(|_| {
            Box::pin(async { Err(ToolError::Message("Extension context required".into())) })
        }),
    );
    definition.label = "Show HTML Page".into();
    definition.prompt_snippet = Some(
        "show_html_page: show a self-contained HTML page (chart/table/diagram) to the reader"
            .into(),
    );
    definition.prompt_guidelines = Some(vec![
        "No network: inline every script, style and image (data: URIs); a CDN <script src> or remote font is refused and leaves the page blank.".into(),
        "Absolute-path local images are inlined after a magic-byte check; a renamed non-image is refused.".into(),
        "Pass screenshots and other images as absolute file paths, never as base64 data: URIs: the page is limited to 512000 characters before images are inlined.".into(),
    ]);
    definition
}

pub fn create_show_html_page_handler(
    pending: Arc<Mutex<PendingHandedPages>>,
) -> ExtensionToolExecutor {
    Arc::new(move |id, args, _signal, _on_update, ctx| {
        let pending = pending.clone();
        Box::pin(async move {
            let html = args
                .get("html")
                .and_then(Value::as_str)
                .filter(|h| !h.is_empty())
                .ok_or_else(|| fail("html is required."))?;
            let title = args
                .get("title")
                .and_then(Value::as_str)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| fail("title is required."))?;
            let height = args.get("height").and_then(Value::as_f64);
            let result =
                write_html_page(html, title, height, &ctx.cwd, host_renders_page()).await?;
            Ok(hand_html_page(id, html, ctx, &pending, result))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn writes_page_and_refuses_missing_or_oversized_pages() {
        let dir = tempfile::tempdir().expect("tempdir");
        let ok = write_html_page("<p>hi</p>", "My Chart", Some(500.0), dir.path(), false)
            .await
            .expect("write");
        assert_eq!(ok.details["imagesInlined"], true);
        assert!(ok.details["path"].as_str().is_some());

        let missing = write_html_page(
            r#"<img src="/no/such/file.png">"#,
            "Missing",
            None,
            dir.path(),
            false,
        )
        .await;
        assert!(missing.is_err());

        let big = "a".repeat(MAX_HTML_CHARACTERS + 1);
        assert!(
            write_html_page(&big, "Big", None, dir.path(), false)
                .await
                .is_err()
        );
    }
}
