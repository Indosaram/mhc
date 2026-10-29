//! Markers that tag agent-injected (non-human) user messages.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

pub const OMO_INTERNAL_INITIATOR_MARKER: &str = "<!-- OMO_INTERNAL_INITIATOR -->";
pub const OMO_INTERNAL_NOREPLY_MARKER: &str = "<!-- OMO_INTERNAL_NOREPLY -->";

fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).unwrap_or_else(|error| panic!("{error}"))
}

static INITIATOR_DETECT: LazyLock<Regex> =
    LazyLock::new(|| compile(r"<!--\s*OMO_INTERNAL_INITIATOR\s*-->"));
static NOREPLY_DETECT: LazyLock<Regex> =
    LazyLock::new(|| compile(r"<!--\s*OMO_INTERNAL_NOREPLY\s*-->"));
static INITIATOR_STRIP: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\n*<!--\s*OMO_INTERNAL_INITIATOR\s*-->\s*"));
static NOREPLY_STRIP: LazyLock<Regex> =
    LazyLock::new(|| compile(r"\n*<!--\s*OMO_INTERNAL_NOREPLY\s*-->\s*"));

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextPartLike {
    pub part_type: Option<String>,
    pub text: Option<String>,
    pub synthetic: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageLike {
    pub role: Option<String>,
    pub info_role: Option<String>,
    pub parts: Option<Vec<TextPartLike>>,
}

impl MessageLike {
    fn effective_role(&self) -> Option<&str> {
        self.info_role.as_deref().or(self.role.as_deref())
    }

    fn text_parts(&self) -> impl Iterator<Item = &TextPartLike> {
        self.parts
            .iter()
            .flatten()
            .filter(|part| is_text_part_like(part))
    }
}

pub fn has_internal_initiator_marker(text: &str) -> bool {
    INITIATOR_DETECT.is_match(text)
}

pub fn has_internal_no_reply_marker(text: &str) -> bool {
    NOREPLY_DETECT.is_match(text)
}

pub fn is_text_part_like(part: &TextPartLike) -> bool {
    part.part_type.as_deref() == Some("text") && part.text.is_some()
}

pub fn is_synthetic_or_internal_text_part(part: &TextPartLike) -> bool {
    is_text_part_like(part)
        && (part.synthetic
            || part
                .text
                .as_deref()
                .is_some_and(has_internal_initiator_marker))
}

pub fn is_real_user_text_part(part: &TextPartLike) -> bool {
    is_text_part_like(part) && !is_synthetic_or_internal_text_part(part)
}

pub fn is_synthetic_or_internal_only_text_parts(parts: Option<&[TextPartLike]>) -> bool {
    let mut text_parts = parts
        .unwrap_or_default()
        .iter()
        .filter(|part| is_text_part_like(part))
        .peekable();
    text_parts.peek().is_some() && text_parts.all(is_synthetic_or_internal_text_part)
}

pub fn is_synthetic_or_internal_user_message(message: &MessageLike) -> bool {
    message.effective_role() == Some("user")
        && is_synthetic_or_internal_only_text_parts(message.parts.as_deref())
}

pub fn is_terminal_no_reply_user_message(message: &MessageLike) -> bool {
    message.effective_role() == Some("user")
        && message.text_parts().any(|part| {
            part.text
                .as_deref()
                .is_some_and(has_internal_no_reply_marker)
        })
}

pub fn is_real_user_message(message: &MessageLike) -> bool {
    message.effective_role() == Some("user") && !is_synthetic_or_internal_user_message(message)
}

pub fn strip_internal_initiator_markers(text: &str) -> String {
    let without_initiator = INITIATOR_STRIP.replace_all(text, "");
    NOREPLY_STRIP
        .replace_all(&without_initiator, "")
        .trim_end()
        .to_string()
}

/// `{type: "text", text}` with the initiator marker appended.
pub fn create_internal_agent_text_part(text: &str) -> Value {
    let clean = strip_internal_initiator_markers(text);
    json!({ "type": "text", "text": format!("{clean}\n{OMO_INTERNAL_INITIATOR_MARKER}") })
}

pub fn create_internal_agent_continuation_text_part(text: &str) -> Value {
    let clean = strip_internal_initiator_markers(text);
    json!({
        "type": "text",
        "text": format!("{clean}\n{OMO_INTERNAL_INITIATOR_MARKER}"),
        "synthetic": true,
        "metadata": { "compaction_continue": true },
    })
}

/// Append the no-reply marker to a text part's `text` unless already present.
pub fn with_internal_no_reply_marker(part: &Value) -> Value {
    let Some(text) = part.get("text").and_then(Value::as_str) else {
        return part.clone();
    };
    if has_internal_no_reply_marker(text) {
        return part.clone();
    }
    let mut next = part.clone();
    next["text"] = Value::String(format!("{text}\n{OMO_INTERNAL_NOREPLY_MARKER}"));
    next
}
