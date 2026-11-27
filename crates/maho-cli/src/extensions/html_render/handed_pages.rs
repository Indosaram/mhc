//! Port of `packages/coding-agent/src/extensions/html-render/handed-pages.ts` (senpi#6921).

use std::collections::{BTreeMap, HashSet};

use maho_ext_api::SessionEntry;

pub const MAX_HANDED_PAGE_CHARACTERS: usize = 2 * 1024 * 1024;
pub const MAX_HANDED_PAGES_CHARACTERS: usize = 6 * 1024 * 1024;
pub const SHOW_HTML_PAGE_NAME: &str = "show_html_page";

pub type PendingHandedPages = BTreeMap<String, usize>;

pub fn inlined_page_characters(
    entries: &[SessionEntry],
    pending: &mut PendingHandedPages,
) -> usize {
    let mut total = 0usize;
    let mut recorded = HashSet::new();
    for entry in entries {
        if entry.kind != "message" {
            continue;
        }
        let Some(message) = entry.data.get("message") else {
            continue;
        };
        if message.get("role").and_then(serde_json::Value::as_str) != Some("toolResult") {
            continue;
        }
        if message.get("toolName").and_then(serde_json::Value::as_str) != Some(SHOW_HTML_PAGE_NAME)
        {
            continue;
        }
        if let Some(call_id) = message
            .get("toolCallId")
            .and_then(serde_json::Value::as_str)
        {
            recorded.insert(call_id.to_owned());
        }
        if let Some(details) = message
            .get("details")
            .and_then(serde_json::Value::as_object)
            && details
                .get("imagesInlined")
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            && let Some(html) = details.get("html").and_then(serde_json::Value::as_str)
        {
            total += html.encode_utf16().count();
        }
    }
    let to_remove: Vec<String> = pending
        .keys()
        .filter(|id| recorded.contains(id.as_str()))
        .cloned()
        .collect();
    for id in to_remove {
        pending.remove(&id);
    }
    for chars in pending.values() {
        total += *chars;
    }
    total
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HandedPageReservation {
    pub inlined: bool,
    pub already_handed: usize,
}

pub fn reserve_handed_page(
    entries: &[SessionEntry],
    pending: &mut PendingHandedPages,
    tool_call_id: &str,
    characters: usize,
) -> HandedPageReservation {
    let already_handed = inlined_page_characters(entries, pending);
    let inlined = already_handed + characters <= MAX_HANDED_PAGES_CHARACTERS;
    if inlined {
        pending.insert(tool_call_id.to_owned(), characters);
    }
    HandedPageReservation {
        inlined,
        already_handed,
    }
}

pub const IMAGES_INLINED: &str = "Local images were inlined.";

pub fn over_session_budget_text(already_handed: usize) -> String {
    format!(
        "Local images were inlined into the saved copy, but this session's pages already carry {already_handed} of the {MAX_HANDED_PAGES_CHARACTERS} characters of images a session can carry to a viewer, so a viewer on another machine will not see this page's images."
    )
}

pub fn handed_page_text(
    path: &str,
    shown: bool,
    images_inlined: bool,
    fits_page: bool,
    already_handed: usize,
    inlined_length: usize,
) -> String {
    let where_text = if shown {
        format!(
            "The page was shown to the reader inline above your reply. A copy is saved at {path}. "
        )
    } else {
        format!("Wrote the page to {path}. It was not displayed in this interface. ")
    };
    if images_inlined {
        return format!("{where_text}{IMAGES_INLINED}");
    }
    if fits_page {
        format!("{where_text}{}", over_session_budget_text(already_handed))
    } else {
        format!(
            "{where_text}Local images were inlined into the saved copy, but with them the page is {inlined_length} characters, over the {MAX_HANDED_PAGE_CHARACTERS} a page can carry to a viewer, so a viewer on another machine will not see the images. Use smaller images to show them everywhere."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn enforces_session_budget_and_clears_pending_once_recorded() {
        let mut pending = PendingHandedPages::new();
        let entry = SessionEntry {
            id: "e1".into(),
            parent_id: None,
            timestamp: "2026-01-01T00:00:00Z".into(),
            kind: "message".into(),
            data: json!({
                "message": {
                    "role": "toolResult",
                    "toolName": "show_html_page",
                    "toolCallId": "call-1",
                    "details": {
                        "imagesInlined": true,
                        "html": "x".repeat(5_500_000)
                    }
                }
            }),
        };
        pending.insert("call-1".into(), 5_500_000);
        let res = reserve_handed_page(&[entry], &mut pending, "call-2", 1_000_000);
        assert!(!res.inlined);
        assert_eq!(res.already_handed, 5_500_000);
        assert!(!pending.contains_key("call-1"));
    }
}
