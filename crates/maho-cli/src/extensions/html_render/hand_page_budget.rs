//! Port of `packages/coding-agent/src/extensions/html-render/hand-page-budget.ts` (senpi#6921, #6932, #6934).

use maho_agent::types::AgentToolResult;
use maho_ai::types::{ContentBlock, TextContent};
use maho_ext_api::SessionEntry;
use serde_json::Value;

use super::handed_pages::{
    PendingHandedPages, SHOW_HTML_PAGE_NAME, handed_page_text, reserve_handed_page,
};

#[derive(Clone, Debug)]
pub struct PageDowngrade {
    pub text: String,
    pub own_text: Option<String>,
    pub details: serde_json::Map<String, Value>,
}

fn older_hand_own_text(downgrade: &PageDowngrade, content: &[ContentBlock]) -> Option<usize> {
    if let Some(path) = downgrade.details.get("path").and_then(Value::as_str)
        && let Some(pos) = content
            .iter()
            .position(|part| matches!(part, ContentBlock::Text(t) if t.text.contains(path)))
    {
        return Some(pos);
    }
    content
        .iter()
        .position(|part| matches!(part, ContentBlock::Text(_)))
}

pub fn apply_page_downgrade(downgrade: &PageDowngrade, result: &mut AgentToolResult) {
    let own = match &downgrade.own_text {
        Some(own_text) => result
            .content
            .iter()
            .position(|part| matches!(part, ContentBlock::Text(t) if t.text == *own_text)),
        None => older_hand_own_text(downgrade, &result.content),
    };
    let text_block = ContentBlock::Text(TextContent {
        text: downgrade.text.clone(),
        audience: None,
        text_signature: None,
    });
    match own {
        Some(idx) => result.content[idx] = text_block,
        None if downgrade.own_text.is_none() => result.content.insert(0, text_block),
        None => result.content.push(text_block),
    }
    result.details = Value::Object(downgrade.details.clone());
}

pub fn compute_page_downgrade(
    tool_name: &str,
    tool_call_id: &str,
    written_html: Option<&str>,
    details: &Value,
    entries: &[SessionEntry],
    pending: &mut PendingHandedPages,
    shown_inline: bool,
) -> Option<PageDowngrade> {
    if tool_name != SHOW_HTML_PAGE_NAME {
        return None;
    }
    let obj = details.as_object()?;
    if obj.get("imagesInlined").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    let inlined_html = obj.get("html").and_then(Value::as_str)?;
    let path = obj.get("path").and_then(Value::as_str)?;
    let written = written_html?;
    let inlined_chars = inlined_html.encode_utf16().count();
    let reservation = reserve_handed_page(entries, pending, tool_call_id, inlined_chars);
    if reservation.inlined {
        return None;
    }
    let text = handed_page_text(
        path,
        shown_inline,
        false,
        true,
        reservation.already_handed,
        inlined_chars,
    );
    let own_text = obj
        .get("resultText")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut next_details = obj.clone();
    next_details.insert("html".to_owned(), Value::String(written.to_owned()));
    next_details.insert("imagesInlined".to_owned(), Value::Bool(false));
    next_details.insert("resultText".to_owned(), Value::String(text.clone()));
    Some(PageDowngrade {
        text,
        own_text,
        details: next_details,
    })
}
