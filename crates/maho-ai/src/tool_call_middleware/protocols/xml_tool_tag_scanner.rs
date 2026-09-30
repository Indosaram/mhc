//! Port of senpi packages/ai/src/tool-call-middleware/protocols/xml-tool-tag-scanner.ts.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlToolTagMatch {
    pub index: usize,
    pub name: String,
    pub tag: String,
    pub self_closing: bool,
}

/// Matches `<\s*name\s*/?\s*>` starting at `from_index`; `self_closing` is set when a `/` sits
/// before the closing `>`.
fn match_tool_tag(text: &str, tool_name: &str, from_index: usize) -> Option<(usize, usize, bool)> {
    let bytes = text.as_bytes();
    let name_bytes = tool_name.as_bytes();
    let mut i = from_index;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if !text.as_bytes()[cursor..].starts_with(name_bytes) {
            i += 1;
            continue;
        }
        cursor += name_bytes.len();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let mut self_closing = false;
        if cursor < bytes.len() && bytes[cursor] == b'/' {
            self_closing = true;
            cursor += 1;
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
        }
        if cursor < bytes.len() && bytes[cursor] == b'>' {
            cursor += 1;
            return Some((i, cursor, self_closing));
        }
        i += 1;
    }
    None
}

pub fn find_self_closing_tool_tag(text: &str, tool_name: &str, from_index: usize) -> Option<(usize, usize, String)> {
    let mut cursor = from_index;
    while let Some((start, end, self_closing)) = match_tool_tag(text, tool_name, cursor) {
        if self_closing {
            let tag = text[start..end].to_string();
            return Some((start, end - start, tag));
        }
        cursor = start + 1;
    }
    None
}

pub fn find_earliest_xml_tool_tag(text: &str, tool_names: &[String]) -> Option<XmlToolTagMatch> {
    let mut earliest: Option<XmlToolTagMatch> = None;
    for tool_name in tool_names {
        let Some((start, end, self_closing)) = match_tool_tag(text, tool_name, 0) else { continue };
        if earliest.as_ref().is_none_or(|e| start < e.index) {
            earliest = Some(XmlToolTagMatch { index: start, name: tool_name.clone(), tag: text[start..end].to_string(), self_closing });
        }
    }
    earliest
}

pub fn get_safe_xml_text_length(text: &str, tool_names: &[String]) -> usize {
    let Some(last_tag_index) = text.rfind('<') else { return text.len() };

    let trailing_candidate = &text[last_tag_index..];
    let has_potential_tool_start = tool_names.iter().any(|tool_name| {
        let candidates = [
            format!("<{tool_name}>"),
            format!("<{tool_name}/>"),
            format!("< {tool_name}>"),
            format!("< {tool_name}/>"),
            format!("<{tool_name} />"),
            format!("< {tool_name} />"),
        ];
        candidates.iter().any(|candidate| candidate.starts_with(trailing_candidate))
    });
    if !has_potential_tool_start {
        return text.len();
    }

    last_tag_index
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_self_closing_tool_tag_matches_with_optional_whitespace() {
        let (index, length, tag) = find_self_closing_tool_tag("before <foo/> after", "foo", 0).expect("match");
        assert_eq!(index, 7);
        assert_eq!(tag, "<foo/>");
        assert_eq!(&"before <foo/> after"[index..index + length], "<foo/>");
        assert!(find_self_closing_tool_tag("<foo>not self closing</foo>", "foo", 0).is_none());
    }

    #[test]
    fn find_earliest_xml_tool_tag_picks_the_earliest_match_and_flags_self_closing() {
        let names = vec!["bar".to_string(), "foo".to_string()];
        let m = find_earliest_xml_tool_tag("text <foo/> then <bar>", &names).expect("match");
        assert_eq!(m.name, "foo");
        assert!(m.self_closing);

        let m2 = find_earliest_xml_tool_tag("text <bar> then <foo/>", &names).expect("match");
        assert_eq!(m2.name, "bar");
        assert!(!m2.self_closing);
    }

    #[test]
    fn find_earliest_xml_tool_tag_returns_none_when_no_tool_matches() {
        assert!(find_earliest_xml_tool_tag("hello", &["foo".to_string()]).is_none());
    }

    #[test]
    fn get_safe_xml_text_length_trims_a_trailing_potential_tool_tag_prefix() {
        let names = vec!["foo".to_string()];
        assert_eq!(get_safe_xml_text_length("hello <fo", &names), 6);
        assert_eq!(get_safe_xml_text_length("hello <div>", &names), 11);
        assert_eq!(get_safe_xml_text_length("no tags", &names), 7);
    }
}
