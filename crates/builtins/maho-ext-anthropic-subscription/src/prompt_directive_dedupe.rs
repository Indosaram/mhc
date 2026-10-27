//! Collapses repeated mode-directive blocks (`<tag>...</tag>`) in a replayed prompt down to the
//! latest copy. Which tags are collapsed is registered at startup by whoever injects them, so this
//! crate names no directive of its own.

use serde_json::{Value, json};
use std::sync::RwLock;

/// A directive wrapped as `<name>...</name>`; earlier copies are replaced by `superseded`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectiveTag { pub open: String, pub close: String, pub superseded: String }

impl DirectiveTag {
    pub fn new(name: &str, label: &str) -> Self {
        Self {
            open: format!("<{name}>"),
            close: format!("</{name}>"),
            superseded: format!("[{label} directive superseded; the latest {label} directive block below applies]"),
        }
    }
}

static REGISTERED: RwLock<Vec<DirectiveTag>> = RwLock::new(Vec::new());

/// Registers a directive tag (`name` without brackets) to collapse in replayed prompts. Idempotent.
pub fn register_directive_tag(name: &str, label: &str) {
    let tag = DirectiveTag::new(name, label);
    let mut registered = REGISTERED.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if !registered.iter().any(|known| known.open == tag.open) { registered.push(tag); }
}

pub fn registered_directive_tags() -> Vec<DirectiveTag> {
    REGISTERED.read().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
}

#[derive(Debug, PartialEq)]
pub struct DedupeResult { pub blocks: Vec<Value>, pub collapsed_directives: usize }

fn text(block: &Value) -> Option<&str> {
    (block["type"] == "text").then(|| block["text"].as_str()).flatten()
}

fn spans(text: &str, tag: &DirectiveTag) -> Vec<std::ops::Range<usize>> {
    let (open, close) = (tag.open.as_str(), tag.close.as_str());
    let mut spans = Vec::new();
    let mut offset = 0;
    while let Some(start) = text[offset..].find(open).map(|start| start + offset) {
        let Some(end) = text[start + open.len()..].find(close).map(|end| start + open.len() + end + close.len()) else { break; };
        spans.push(start..end);
        offset = end;
    }
    spans
}

fn has_nested_directive(blocks: &[Value], tag: &DirectiveTag) -> bool {
    let (open_tag, close_tag) = (tag.open.as_str(), tag.close.as_str());
    let mut depth: usize = 0;
    for text in blocks.iter().filter_map(text) {
        let mut offset = 0;
        loop {
            let open = text[offset..].find(open_tag);
            let close = text[offset..].find(close_tag);
            match (open, close) {
                (Some(open), close) if close.is_none_or(|close| open < close) => {
                    depth += 1;
                    if depth > 1 { return true; }
                    offset += open + open_tag.len();
                }
                (_, Some(close)) => {
                    depth = depth.saturating_sub(1);
                    offset += close + close_tag.len();
                }
                (None, None) => break,
                (Some(_), None) => unreachable!("opening tags are handled by the guarded arm"),
            }
        }
    }
    false
}

pub fn serialized_payload_bytes(blocks: &[Value]) -> usize {
    blocks.iter().filter_map(text).map(str::len).sum()
}

/// Collapses every registered directive tag (see [`register_directive_tag`]).
pub fn dedupe_registered_directive_blocks(blocks: &[Value]) -> DedupeResult {
    let mut result = DedupeResult { blocks: blocks.to_vec(), collapsed_directives: 0 };
    for tag in registered_directive_tags() {
        let next = dedupe_directive_blocks(&result.blocks, &tag);
        result = DedupeResult { blocks: next.blocks, collapsed_directives: result.collapsed_directives + next.collapsed_directives };
    }
    result
}

pub fn dedupe_directive_blocks(blocks: &[Value], tag: &DirectiveTag) -> DedupeResult {
    if has_nested_directive(blocks, tag) { return DedupeResult { blocks: blocks.to_vec(), collapsed_directives: 0 }; }
    let total: usize = blocks.iter().filter_map(text).map(|text| spans(text, tag).len()).sum();
    if total == 0 { return DedupeResult { blocks: blocks.to_vec(), collapsed_directives: 0 }; }
    let mut remaining = total;
    let next = blocks.iter().map(|block| {
        let Some(text) = text(block) else { return block.clone(); };
        let mut replaced = String::new();
        let mut offset = 0;
        for span in spans(text, tag) {
            replaced.push_str(&text[offset..span.start]);
            remaining -= 1;
            replaced.push_str(if remaining == 0 { &text[span.clone()] } else { tag.superseded.as_str() });
            offset = span.end;
        }
        replaced.push_str(&text[offset..]);
        json!({"type":"text", "text":replaced})
    }).collect();
    DedupeResult { blocks: next, collapsed_directives: total - 1 }
}

#[cfg(test)]
mod tests {
    use super::*;
    const OPEN: &str = "<focus-mode>";
    const CLOSE: &str = "</focus-mode>";
    const SUPERSEDED: &str = "[focus directive superseded; the latest focus directive block below applies]";
    fn dedupe(blocks: &[Value]) -> DedupeResult { dedupe_directive_blocks(blocks, &DirectiveTag::new("focus-mode", "focus")) }
    fn block(text: &str) -> Value { json!({"type":"text", "text":text}) }
    fn directive(body: &str) -> String { format!("{OPEN}{body}{CLOSE}") }
    #[test]
    fn repeated_blocks_keep_most_recent() {
        let blocks = vec![block(&directive("first")), block("ok"), block(&directive("second")), block("ok2"), block(&directive("last"))];
        let result = dedupe(&blocks);
        assert_eq!(result.collapsed_directives, 2);
        assert_eq!(result.blocks, vec![block(SUPERSEDED), block("ok"), block(SUPERSEDED), block("ok2"), block(&directive("last"))]);
    }
    #[test]
    fn surrounding_text_intact() {
        let blocks = vec![block(&format!("do X\n{}", directive("first"))), block(&directive("last"))];
        assert_eq!(dedupe(&blocks).blocks[0], block(&format!("do X\n{SUPERSEDED}")));
    }
    #[test]
    fn no_spans_identical() {
        let blocks = vec![block("Find it"), block("ok"), block("Explain")];
        assert_eq!(dedupe(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn lone_open_untouched() {
        let blocks = vec![block(&format!("lone {OPEN} mention"))];
        assert_eq!(dedupe(&blocks).blocks, blocks);
    }
    #[test]
    fn only_final_copy_preserved() {
        let blocks = vec![block("earlier turn"), block("ok"), block(&directive("last"))];
        assert_eq!(dedupe(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn last_body_preserved_earlier_bodies_removed() {
        let blocks = vec![block(&format!("{} {} {}", directive("FIRST"), directive("SECOND"), directive("LAST")))];
        let result = dedupe(&blocks);
        assert_eq!(result.collapsed_directives, 2);
        assert_eq!(result.blocks, vec![block(&format!("{SUPERSEDED} {SUPERSEDED} {}", directive("LAST")))]);
        assert!(blocks[0]["text"].as_str().unwrap().contains("FIRST"));
    }
    #[test]
    fn split_nested_tags_fail_closed() {
        let blocks = vec![block(&directive("PRIOR")), block(&format!("{OPEN}outer ")), block(&directive("inner")), block(&format!(" tail{CLOSE}"))];
        assert_eq!(dedupe(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn flat_directives_in_separate_blocks() {
        let blocks = vec![block(&directive("EARLIER")), block("interleaved prose"), block(&directive("LATEST"))];
        assert_eq!(dedupe(&blocks).blocks, vec![block(SUPERSEDED), block("interleaved prose"), block(&directive("LATEST"))]);
    }
    #[test]
    fn nested_tags_untouched() {
        let blocks = vec![block(&directive("prior")), block(&format!("{OPEN}outer {OPEN}inner{CLOSE} tail{CLOSE}"))];
        assert_eq!(dedupe(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn assistant_echoes_also_collapsed() {
        let blocks = vec![block(&directive("body")), block(&format!("echo {}", directive("body"))), block(&directive("body"))];
        let result = dedupe(&blocks);
        assert_eq!(result.collapsed_directives, 2);
        assert_eq!(result.blocks[1], block(&format!("echo {SUPERSEDED}")));
    }
    #[test]
    fn split_flat_tags_never_form_span() {
        let blocks = vec![block(OPEN), json!({"type":"image", "source":{"type":"base64"}}), block(CLOSE)];
        assert_eq!(dedupe(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn registered_tags_are_collapsed_and_registration_is_idempotent() {
        register_directive_tag("registry-test-mode", "registry test");
        register_directive_tag("registry-test-mode", "registry test");
        assert_eq!(registered_directive_tags().iter().filter(|tag| tag.open == "<registry-test-mode>").count(), 1);
        let directive = |body: &str| format!("<registry-test-mode>{body}</registry-test-mode>");
        let result = dedupe_registered_directive_blocks(&[block(&directive("a")), block(&directive("b"))]);
        assert_eq!(result.collapsed_directives, 1);
        assert_eq!(result.blocks[0], block("[registry test directive superseded; the latest registry test directive block below applies]"));
        assert_eq!(result.blocks[1], block(&directive("b")));
    }
    #[test]
    fn payload_counts_utf8_not_utf16() {
        assert_eq!(serialized_payload_bytes(&[block("한글😀"), json!({"type":"image"})]), 10);
    }
}
