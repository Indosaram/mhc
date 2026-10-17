//! Emergency head/tail truncation preserves result metadata and marker compatibility.
use maho_ext_api::{ToolContent, ToolResult};

fn marker(text: &str) -> Option<&str> {
    for (start, _) in text.match_indices("<truncated:") {
        let rest = text.get(start + 11..)?;
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 || !rest.get(digits..)?.starts_with(" bytes original") { continue; }
        let end = start + 11 + rest.find('>')? + 1;
        return text.get(start..end);
    }
    None
}
fn approximate_tokens(text: &str) -> usize { text.encode_utf16().count().div_ceil(4) }
fn from_utf16_lossless(units:&[u16])->String {
    let mut output=String::new();
    for decoded in char::decode_utf16(units.iter().copied()) {
        match decoded {
            Ok(character)=>output.push(character),
            Err(error)=>output.push_str(&format!("\\u{:04x}",error.unpaired_surrogate())),
        }
    }
    output
}
fn char_boundary(units: &[u16], index: usize, round_up: bool) -> usize {
    if index == 0 || index >= units.len() || !(0xDC00..=0xDFFF).contains(&units[index]) || !(0xD800..=0xDBFF).contains(&units[index - 1]) { index }
    else if round_up { index + 1 } else { index - 1 }
}
fn total_tokens(results: &[ToolResult]) -> usize {
    results.iter().flat_map(|result| &result.content).map(|block| match block {
        ToolContent::Text {text,..} => approximate_tokens(text),
        ToolContent::Image {data,..} => approximate_tokens(data),
    }).sum()
}
pub fn truncate_oversized_tool_results(results: &[ToolResult]) -> Vec<ToolResult> {
    results.iter().map(|result| {
        let mut next = result.clone();
        for block in &mut next.content {
            if let ToolContent::Text {text,..} = block {
                let bytes = text.len();
                if bytes <= 4096 || marker(text).is_some() { continue; }
                let units: Vec<u16> = text.encode_utf16().collect();
                let head_end = char_boundary(&units, 800.min(units.len()), false);
                let tail_start = char_boundary(&units, units.len().saturating_sub(400), true);
                let head = from_utf16_lossless(&units[..head_end]);
                let tail = from_utf16_lossless(&units[tail_start..]);
                *text = format!("{head}\n<truncated:{bytes} bytes original; middle elided to save context - re-run this tool with a narrower range (read offset/limit or a filtered command) to retrieve the elided content>\n{tail}");
            }
        }
        next
    }).collect()
}
pub fn pre_prune_tool_outputs_to_budget(results: &[ToolResult], target_tokens: usize) -> Vec<ToolResult> {
    if total_tokens(results) <= target_tokens { return results.to_vec(); }
    let mut first_pass = truncate_oversized_tool_results(results);
    if total_tokens(&first_pass) <= target_tokens { return first_pass; }
    for result in &mut first_pass {
        for block in &mut result.content {
            if let ToolContent::Text {text,..} = block
                && let Some(mark) = marker(text) {
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
    fn utf16_slice_preserves_unpaired_surrogate_as_escape() {
        assert_eq!(from_utf16_lossless(&[0xdfff]),"\\udfff");
    }
}
