//! Rendering and composition of the operating-rules block in system prompts.
//!
//! Upstream counterpart: `rules-block.ts`.

pub const GATEWAY_RULES_SENTINEL_BEGIN: &str = "<!-- omo-gateway:rules:begin -->";
pub const GATEWAY_RULES_SENTINEL_END: &str = "<!-- omo-gateway:rules:end -->";

/// Escape untrusted rule text so tags and sentinels cannot break prompt framing.
pub fn escape_rule_text(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Render the XML operating-rules block.
pub fn render_operating_rules_block(version: &str, behavioral: &[String]) -> String {
    let escaped_version = escape_rule_text(version).replace('"', "&quot;");
    let mut lines = vec![format!(r#"<operating-rules version="{escaped_version}">"#)];
    for text in behavioral {
        lines.push(format!("- {}", escape_rule_text(text)));
    }
    lines.push("</operating-rules>".to_string());
    lines.join("
")
}

/// Wrap a rendered operating-rules block with sentinel comments.
pub fn mark_gateway_rules_block(block: &str) -> String {
    format!("{GATEWAY_RULES_SENTINEL_BEGIN}
{block}
{GATEWAY_RULES_SENTINEL_END}")
}

/// Compose the rules block into the system prompt.
///
/// If a sentinel region already exists (carried back from a previous turn or preview),
/// replace it in-place. Otherwise, append after existing content separated by two newlines.
pub fn compose_gateway_rules_block(system_prompt: &str, block: &str) -> String {
    let marked = mark_gateway_rules_block(block);
    if let Some(start_idx) = system_prompt.find(GATEWAY_RULES_SENTINEL_BEGIN)
        && let Some(end_offset) = system_prompt[start_idx..].find(GATEWAY_RULES_SENTINEL_END)
    {
        let end_idx = start_idx + end_offset + GATEWAY_RULES_SENTINEL_END.len();
        let mut result = String::with_capacity(system_prompt.len() + marked.len());
        result.push_str(&system_prompt[..start_idx]);
        result.push_str(&marked);
        result.push_str(&system_prompt[end_idx..]);
        return result;
    }
    format!("{}

{}", system_prompt.trim_end(), marked)
}
