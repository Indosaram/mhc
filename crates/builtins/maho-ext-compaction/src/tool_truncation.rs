//! Emergency head/tail truncation preserves result metadata and marker compatibility.
use maho_ext_api::{ToolContent, ToolResult};

fn marker(text: &str) -> Option<&str> {
    for (start, _) in text.match_indices("<truncated:") {
        let rest = text.get(start + 11..)?;
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !rest.get(digits..)?.starts_with(" bytes original") {
            continue;
        }
        let end = start + 11 + rest.find('>')? + 1;
        return text.get(start..end);
    }
    None
}
fn approximate_tokens(text: &str) -> usize {
    text.encode_utf16().count().div_ceil(4)
}
/// JS `text.slice` over UTF-16 units. A cut through a surrogate pair leaves a lone surrogate in JS;
/// a Rust string cannot hold one, so it decodes to U+FFFD, which is also one UTF-16 unit and keeps
/// the head/tail lengths and the token estimate equal to upstream.
fn slice_units(units: &[u16]) -> String {
    String::from_utf16_lossy(units)
}
fn total_tokens(results: &[ToolResult]) -> usize {
    results
        .iter()
        .flat_map(|result| &result.content)
        .map(|block| match block {
            ToolContent::Text { text, .. } => approximate_tokens(text),
            ToolContent::Image { data, .. } => approximate_tokens(data),
        })
        .sum()
}
pub fn truncate_oversized_tool_results(results: &[ToolResult]) -> Vec<ToolResult> {
    results.iter().map(|result| {
        let mut next = result.clone();
        for block in &mut next.content {
            if let ToolContent::Text {text,..} = block {
                let bytes = text.len();
                if bytes <= 4096 || marker(text).is_some() { continue; }
                let units: Vec<u16> = text.encode_utf16().collect();
                let head = slice_units(&units[..800.min(units.len())]);
                let tail = slice_units(&units[units.len().saturating_sub(400)..]);
                *text = format!("{head}\n<truncated:{bytes} bytes original; middle elided to save context - re-run this tool with a narrower range (read offset/limit or a filtered command) to retrieve the elided content>\n{tail}");
            }
        }
        next
    }).collect()
}
pub fn pre_prune_tool_outputs_to_budget(
    results: &[ToolResult],
    target_tokens: usize,
) -> Vec<ToolResult> {
    if total_tokens(results) <= target_tokens {
        return results.to_vec();
    }
    let mut first_pass = truncate_oversized_tool_results(results);
    if total_tokens(&first_pass) <= target_tokens {
        return first_pass;
    }
    for result in &mut first_pass {
        for block in &mut result.content {
            if let ToolContent::Text { text, .. } = block
                && let Some(mark) = marker(text)
            {
                *text = mark.to_owned();
            }
        }
    }
    first_pass
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_surrogate_keeps_its_single_utf16_unit() {
        assert_eq!(slice_units(&[0xdfff]).encode_utf16().count(), 1);
    }
}
