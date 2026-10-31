//! `show_html_page`: show a finished self-contained HTML page to the reader.
use crate::{bootstrap::inject_html_render_bootstrap, images::inline_local_images};
use maho_agent::types::AgentToolResult;
use maho_ai::types::{ContentBlock, TextContent};
use maho_ext_api::{ExtensionApi, ExtensionFailure, ToolDefinition, ToolError};
use serde_json::{Value, json};
use std::sync::{Arc, OnceLock};

// The same input limit as the desktop's html_render. Images belong as absolute file paths, which are
// inlined after the check, so the document itself stays small: it travels in the tool call and again in
// the result details.
const MAX_HTML_CHARACTERS: usize = 512_000;
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
            "title": {"type": "string", "description": "Short name for the page.", "minLength": 1, "maxLength": 200},
            "height": {"type": "integer", "description": "Frame height hint in CSS pixels, 80-2000. Defaults to the page's natural height."}
        },
        "required": ["html", "title"]
    })
}

pub const SHOW_HTML_PAGE_DESCRIPTION: &str = "Show a finished self-contained HTML page (chart, table, diagram, mockup) to the reader. Load the visualize skill before building the page. No network: inline every script, style and image (data: URIs). A <script src>, stylesheet link, font, image or fetch that points at a URL (a CDN included) is refused, and the page comes out blank or broken. Local images written as absolute file paths are inlined automatically after a byte check. In the desktop thread this renders inline above your reply; elsewhere it is written to a file you can open in the desktop.";

fn fail(message: impl Into<String>) -> ExtensionFailure {
    ExtensionFailure::new(message.into())
}

/// The limit is checked here rather than in the schema so the refusal says what to do: a report usually
/// goes over it because its screenshots are base64, and the fix is to pass them as absolute file paths,
/// which this tool inlines itself after the check (senpi#3418).
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
    let images = pattern
        .find_iter(html)
        .map(|found| found.as_str().encode_utf16().count())
        .collect::<Vec<_>>();
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

fn clamp_height(height: Option<i64>) -> Option<i64> {
    height.map(|height| height.clamp(80, 2000))
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
    let name = collapsed.trim().chars().take(60).collect::<String>();
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

pub fn register_show_html_page_tool(api: &mut ExtensionApi) {
    let mut definition = ToolDefinition::new(
        "show_html_page",
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
    if let Err(error) = api.register_tool_with_extension_context(
        definition,
        Arc::new(|_id, args, _signal, _update, ctx| {
            Box::pin(async move {
                let html = args.get("html").and_then(Value::as_str).filter(|html| !html.is_empty()).ok_or_else(|| fail("html is required."))?;
                let title = args.get("title").and_then(Value::as_str).filter(|title| !title.is_empty()).ok_or_else(|| fail("title is required."))?;
                assert_within_input_limit(html)?;
                let (inlined, missing) = inline_local_images(html).await.map_err(|error| fail(error.to_string()))?;
                let prepared = inject_html_render_bootstrap(&inlined);
                let height = clamp_height(args.get("height").and_then(Value::as_f64).map(|height| height.round() as i64));
                let dir = ctx.cwd.join(OUTPUT_DIR);
                tokio::fs::create_dir_all(&dir).await.map_err(|error| fail(error.to_string()))?;
                let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_millis());
                let path = dir.join(format!("{}-{}.html", safe_name(title), base36(millis)));
                tokio::fs::write(&path, prepared).await.map_err(|error| fail(error.to_string()))?;
                let path = path.to_string_lossy().into_owned();
                let note = if missing.is_empty() {
                    "Local images were inlined.".to_owned()
                } else {
                    format!("Some local images could not be read and were left as written: {}.", missing.join(", "))
                };
                let mut details = json!({"path": path, "title": title, "height": height, "prepared": true, "html": html});
                if !missing.is_empty() {
                    details["missingImages"] = json!(missing);
                }
                Ok(AgentToolResult {
                    content: vec![ContentBlock::Text(TextContent {
                        text: format!("Wrote the page to {path}. Open it in the desktop to see it rendered with your theme. {note}"),
                        audience: None,
                        text_signature: None,
                    })],
                    details,
                    structured_content: None,
                    usage: None,
                    nested_calls: None,
                    added_tool_names: None,
                    terminate: None,
                    is_error: None,
                })
            })
        }),
    ) {
        std::panic::panic_any(error);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oversized_page_names_the_base64_images_and_the_fix() {
        let image = format!("data:image/png;base64,{}", "A".repeat(600_000));
        let message = assert_within_input_limit(&format!("<img src=\"{image}\">"))
            .expect_err("over the limit")
            .message;
        assert!(message.contains("1 base64 images make up"));
        assert!(assert_within_input_limit("<p>small</p>").is_ok());
    }

    #[test]
    fn height_is_clamped_and_names_are_filesystem_safe() {
        assert_eq!(clamp_height(Some(10)), Some(80));
        assert_eq!(clamp_height(Some(9000)), Some(2000));
        assert_eq!(clamp_height(None), None);
        assert_eq!(safe_name("a/b: c*"), "a b c");
        assert_eq!(safe_name("///"), "page");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
    }

    #[test]
    fn schema_requires_html_and_title() {
        assert_eq!(params_schema()["required"], json!(["html", "title"]));
    }
}
