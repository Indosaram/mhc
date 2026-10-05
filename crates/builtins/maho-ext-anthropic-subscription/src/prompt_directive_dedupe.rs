use serde_json::{Value, json};

const OPEN: &str = "<ultrawork-mode>";
const CLOSE: &str = "</ultrawork-mode>";
const SUPERSEDED: &str = "[ultrawork directive superseded; the latest ultrawork directive block below applies]";

#[derive(Debug, PartialEq)]
pub struct DedupeResult { pub blocks: Vec<Value>, pub collapsed_directives: usize }

fn text(block: &Value) -> Option<&str> {
    (block["type"] == "text").then(|| block["text"].as_str()).flatten()
}

fn spans(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    let mut offset = 0;
    while let Some(start) = text[offset..].find(OPEN).map(|start| start + offset) {
        let Some(end) = text[start + OPEN.len()..].find(CLOSE).map(|end| start + OPEN.len() + end + CLOSE.len()) else { break; };
        spans.push(start..end);
        offset = end;
    }
    spans
}

fn has_nested_directive(blocks: &[Value]) -> bool {
    let mut depth: usize = 0;
    for text in blocks.iter().filter_map(text) {
        let mut offset = 0;
        loop {
            let open = text[offset..].find(OPEN);
            let close = text[offset..].find(CLOSE);
            match (open, close) {
                (Some(open), close) if close.is_none_or(|close| open < close) => {
                    depth += 1;
                    if depth > 1 { return true; }
                    offset += open + OPEN.len();
                }
                (_, Some(close)) => {
                    depth = depth.saturating_sub(1);
                    offset += close + CLOSE.len();
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

pub fn dedupe_ultrawork_blocks(blocks: &[Value]) -> DedupeResult {
    if has_nested_directive(blocks) { return DedupeResult { blocks: blocks.to_vec(), collapsed_directives: 0 }; }
    let total: usize = blocks.iter().filter_map(text).map(|text| spans(text).len()).sum();
    if total == 0 { return DedupeResult { blocks: blocks.to_vec(), collapsed_directives: 0 }; }
    let mut remaining = total;
    let next = blocks.iter().map(|block| {
        let Some(text) = text(block) else { return block.clone(); };
        let mut replaced = String::new();
        let mut offset = 0;
        for span in spans(text) {
            replaced.push_str(&text[offset..span.start]);
            remaining -= 1;
            replaced.push_str(if remaining == 0 { &text[span.clone()] } else { SUPERSEDED });
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
    fn block(text: &str) -> Value { json!({"type":"text", "text":text}) }
    fn directive(body: &str) -> String { format!("{OPEN}{body}{CLOSE}") }
    #[test]
    fn repeated_blocks_keep_most_recent() {
        let blocks = vec![block(&directive("first")), block("ok"), block(&directive("second")), block("ok2"), block(&directive("last"))];
        let result = dedupe_ultrawork_blocks(&blocks);
        assert_eq!(result.collapsed_directives, 2);
        assert_eq!(result.blocks, vec![block(SUPERSEDED), block("ok"), block(SUPERSEDED), block("ok2"), block(&directive("last"))]);
    }
    #[test]
    fn surrounding_text_intact() {
        let blocks = vec![block(&format!("do X\n{}", directive("first"))), block(&directive("last"))];
        assert_eq!(dedupe_ultrawork_blocks(&blocks).blocks[0], block(&format!("do X\n{SUPERSEDED}")));
    }
    #[test]
    fn no_spans_identical() {
        let blocks = vec![block("Find it"), block("ok"), block("Explain")];
        assert_eq!(dedupe_ultrawork_blocks(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn lone_open_untouched() {
        let blocks = vec![block(&format!("lone {OPEN} mention"))];
        assert_eq!(dedupe_ultrawork_blocks(&blocks).blocks, blocks);
    }
    #[test]
    fn only_final_copy_preserved() {
        let blocks = vec![block("earlier turn"), block("ok"), block(&directive("last"))];
        assert_eq!(dedupe_ultrawork_blocks(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn last_body_preserved_earlier_bodies_removed() {
        let blocks = vec![block(&format!("{} {} {}", directive("FIRST"), directive("SECOND"), directive("LAST")))];
        let result = dedupe_ultrawork_blocks(&blocks);
        assert_eq!(result.collapsed_directives, 2);
        assert_eq!(result.blocks, vec![block(&format!("{SUPERSEDED} {SUPERSEDED} {}", directive("LAST")))]);
        assert!(blocks[0]["text"].as_str().unwrap().contains("FIRST"));
    }
    #[test]
    fn split_nested_tags_fail_closed() {
        let blocks = vec![block(&directive("PRIOR")), block(&format!("{OPEN}outer ")), block(&directive("inner")), block(&format!(" tail{CLOSE}"))];
        assert_eq!(dedupe_ultrawork_blocks(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn flat_directives_in_separate_blocks() {
        let blocks = vec![block(&directive("EARLIER")), block("interleaved prose"), block(&directive("LATEST"))];
        assert_eq!(dedupe_ultrawork_blocks(&blocks).blocks, vec![block(SUPERSEDED), block("interleaved prose"), block(&directive("LATEST"))]);
    }
    #[test]
    fn nested_tags_untouched() {
        let blocks = vec![block(&directive("prior")), block(&format!("{OPEN}outer {OPEN}inner{CLOSE} tail{CLOSE}"))];
        assert_eq!(dedupe_ultrawork_blocks(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn assistant_echoes_also_collapsed() {
        let blocks = vec![block(&directive("body")), block(&format!("echo {}", directive("body"))), block(&directive("body"))];
        let result = dedupe_ultrawork_blocks(&blocks);
        assert_eq!(result.collapsed_directives, 2);
        assert_eq!(result.blocks[1], block(&format!("echo {SUPERSEDED}")));
    }
    #[test]
    fn split_flat_tags_never_form_span() {
        let blocks = vec![block(OPEN), json!({"type":"image", "source":{"type":"base64"}}), block(CLOSE)];
        assert_eq!(dedupe_ultrawork_blocks(&blocks), DedupeResult { blocks, collapsed_directives: 0 });
    }
    #[test]
    fn payload_counts_utf8_not_utf16() {
        assert_eq!(serialized_payload_bytes(&[block("한글😀"), json!({"type":"image"})]), 10);
    }
}
