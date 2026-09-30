//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/invoke-tag-syntax.ts.

use super::xml_entities::decode_xml_entities;

const ANTML_NAMESPACE: &str = "antml:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagMatch {
    pub index: usize,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterOpenTagMatch {
    pub index: usize,
    pub length: usize,
    pub name: String,
}

pub enum ParameterBoundary {
    ParameterClose(TagMatch),
    InvokeClose(TagMatch),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeOpenTagMatch {
    pub index: usize,
    pub length: usize,
    pub tool_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeParameter {
    pub name: String,
    pub raw_value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvokeBlockMatch {
    pub content_end: usize,
    pub end: usize,
    pub parameters: Option<Vec<InvokeParameter>>,
}

fn is_ws(c: char) -> bool {
    c.is_whitespace()
}

// Scans for `<[antml:]invoke name="..."` (or `'...'`) `>` starting at `from_index`.
fn find_invoke_open_tag_from(text: &str, from_index: usize) -> Option<InvokeOpenTagMatch> {
    let bytes: Vec<char> = text.chars().collect();
    let mut i = from_index;
    while i < bytes.len() {
        if bytes[i] != '<' {
            i += 1;
            continue;
        }
        let start = i;
        let mut cursor = i + 1;
        while cursor < bytes.len() && is_ws(bytes[cursor]) {
            cursor += 1;
        }
        let ns_slice: String = bytes[cursor..bytes.len().min(cursor + ANTML_NAMESPACE.len())].iter().collect();
        if ns_slice == ANTML_NAMESPACE {
            cursor += ANTML_NAMESPACE.len();
        }
        let word_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_alphabetic() {
            cursor += 1;
        }
        let word: String = bytes[word_start..cursor].iter().collect();
        if word != "invoke" {
            i += 1;
            continue;
        }
        let after_word = cursor;
        if !(after_word < bytes.len() && is_ws(bytes[after_word])) {
            i += 1;
            continue;
        }
        while cursor < bytes.len() && is_ws(bytes[cursor]) {
            cursor += 1;
        }
        let name_kw_start = cursor;
        while cursor < bytes.len() && bytes[cursor].is_alphabetic() {
            cursor += 1;
        }
        let name_kw: String = bytes[name_kw_start..cursor].iter().collect();
        if name_kw != "name" {
            i += 1;
            continue;
        }
        while cursor < bytes.len() && is_ws(bytes[cursor]) {
            cursor += 1;
        }
        if !(cursor < bytes.len() && bytes[cursor] == '=') {
            i += 1;
            continue;
        }
        cursor += 1;
        while cursor < bytes.len() && is_ws(bytes[cursor]) {
            cursor += 1;
        }
        if !(cursor < bytes.len() && (bytes[cursor] == '"' || bytes[cursor] == '\'')) {
            i += 1;
            continue;
        }
        let quote = bytes[cursor];
        cursor += 1;
        let value_start = cursor;
        while cursor < bytes.len() && bytes[cursor] != quote {
            cursor += 1;
        }
        if cursor >= bytes.len() {
            i += 1;
            continue;
        }
        let value: String = bytes[value_start..cursor].iter().collect();
        cursor += 1;
        while cursor < bytes.len() && is_ws(bytes[cursor]) {
            cursor += 1;
        }
        if !(cursor < bytes.len() && bytes[cursor] == '>') {
            i += 1;
            continue;
        }
        cursor += 1;
        let byte_start = char_index_to_byte(text, start);
        let byte_end = char_index_to_byte(text, cursor);
        return Some(InvokeOpenTagMatch {
            index: byte_start,
            length: byte_end - byte_start,
            tool_name: decode_xml_entities(&value),
        });
    }
    None
}

fn char_index_to_byte(text: &str, char_index: usize) -> usize {
    text.char_indices().nth(char_index).map(|(b, _)| b).unwrap_or(text.len())
}

fn byte_to_char_index(text: &str, byte_index: usize) -> usize {
    text[..byte_index.min(text.len())].chars().count()
}

pub fn find_invoke_open_tag(text: &str, from_index: usize) -> Option<InvokeOpenTagMatch> {
    let char_from = byte_to_char_index(text, from_index);
    find_invoke_open_tag_from(text, char_from)
}

pub fn find_incomplete_invoke_open_tag(text: &str, from_index: usize) -> Option<InvokeOpenTagMatch> {
    let index = from_index;
    let candidate = &text[index.min(text.len())..];
    let chars: Vec<char> = candidate.chars().collect();
    let mut cursor = 0usize;
    if cursor >= chars.len() || chars[cursor] != '<' {
        return None;
    }
    cursor += 1;
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    let ns: String = chars[cursor..chars.len().min(cursor + ANTML_NAMESPACE.len())].iter().collect();
    if ns == ANTML_NAMESPACE {
        cursor += ANTML_NAMESPACE.len();
    }
    let word_start = cursor;
    while cursor < chars.len() && chars[cursor].is_alphabetic() {
        cursor += 1;
    }
    let word: String = chars[word_start..cursor].iter().collect();
    if word != "invoke" || !(cursor < chars.len() && is_ws(chars[cursor])) {
        return None;
    }
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    let name_kw_start = cursor;
    while cursor < chars.len() && chars[cursor].is_alphabetic() {
        cursor += 1;
    }
    let name_kw: String = chars[name_kw_start..cursor].iter().collect();
    if name_kw != "name" {
        return None;
    }
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    if !(cursor < chars.len() && chars[cursor] == '=') {
        return None;
    }
    cursor += 1;
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    let quote = *chars.get(cursor)?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let value_start = cursor + 1;
    let closing = chars[value_start..].iter().position(|&c| c == quote);
    match closing {
        None => {
            let tool_name = decode_xml_entities(&chars[value_start..].iter().collect::<String>());
            if tool_name.is_empty() { None } else { Some(InvokeOpenTagMatch { index, length: candidate.len(), tool_name }) }
        }
        Some(rel) => {
            let closing_index = value_start + rel;
            let tool_name = decode_xml_entities(&chars[value_start..closing_index].iter().collect::<String>());
            let rest: String = chars[closing_index + 1..].iter().collect();
            if tool_name.is_empty() || !rest.trim().is_empty() {
                None
            } else {
                Some(InvokeOpenTagMatch { index, length: candidate.len(), tool_name })
            }
        }
    }
}

fn find_tag_close(tag: &str, text: &str, from_char: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let tag_chars: Vec<char> = tag.chars().collect();
    let mut i = from_char;
    'outer: while i + tag_chars.len() <= chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
        while cursor < chars.len() && is_ws(chars[cursor]) {
            cursor += 1;
        }
        if !(cursor < chars.len() && chars[cursor] == '/') {
            i += 1;
            continue;
        }
        cursor += 1;
        while cursor < chars.len() && is_ws(chars[cursor]) {
            cursor += 1;
        }
        let ns: String = chars[cursor..chars.len().min(cursor + ANTML_NAMESPACE.len())].iter().collect();
        if ns == ANTML_NAMESPACE {
            cursor += ANTML_NAMESPACE.len();
        }
        let word_start = cursor;
        while cursor < chars.len() && chars[cursor].is_alphabetic() {
            cursor += 1;
        }
        let word: String = chars[word_start..cursor].iter().collect();
        if word != tag {
            i += 1;
            continue 'outer;
        }
        while cursor < chars.len() && is_ws(chars[cursor]) {
            cursor += 1;
        }
        if !(cursor < chars.len() && chars[cursor] == '>') {
            i += 1;
            continue;
        }
        cursor += 1;
        return Some((i, cursor - i));
    }
    None
}

fn find_invoke_close_tag(text: &str, from_char: usize) -> Option<(usize, usize)> {
    find_tag_close("invoke", text, from_char)
}

fn find_parameter_close_tag(text: &str, from_char: usize) -> Option<(usize, usize)> {
    find_tag_close("parameter", text, from_char)
}

fn find_nested_invoke_open(text: &str, from_char: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = from_char;
    while i < chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
        while cursor < chars.len() && is_ws(chars[cursor]) {
            cursor += 1;
        }
        let ns: String = chars[cursor..chars.len().min(cursor + ANTML_NAMESPACE.len())].iter().collect();
        if ns == ANTML_NAMESPACE {
            cursor += ANTML_NAMESPACE.len();
        }
        let word_start = cursor;
        while cursor < chars.len() && chars[cursor].is_alphabetic() {
            cursor += 1;
        }
        let word: String = chars[word_start..cursor].iter().collect();
        if word != "invoke" {
            i += 1;
            continue;
        }
        while cursor < chars.len() && chars[cursor] != '>' {
            cursor += 1;
        }
        if cursor >= chars.len() {
            i += 1;
            continue;
        }
        cursor += 1;
        return Some((i, cursor - i));
    }
    None
}

fn find_parameter_markup(text: &str, from_char: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = from_char;
    while i < chars.len() {
        if chars[i] != '<' {
            i += 1;
            continue;
        }
        let mut cursor = i + 1;
        while cursor < chars.len() && is_ws(chars[cursor]) {
            cursor += 1;
        }
        if cursor < chars.len() && chars[cursor] == '/' {
            cursor += 1;
            while cursor < chars.len() && is_ws(chars[cursor]) {
                cursor += 1;
            }
        }
        let ns: String = chars[cursor..chars.len().min(cursor + ANTML_NAMESPACE.len())].iter().collect();
        if ns == ANTML_NAMESPACE {
            cursor += ANTML_NAMESPACE.len();
        }
        let word_start = cursor;
        while cursor < chars.len() && chars[cursor].is_alphabetic() {
            cursor += 1;
        }
        let word: String = chars[word_start..cursor].iter().collect();
        if word == "parameter" {
            return Some((i, 1));
        }
        i += 1;
    }
    None
}

pub fn find_parameter_open_tag_at(text: &str, char_index: usize) -> Option<ParameterOpenTagMatch> {
    let chars: Vec<char> = text.chars().collect();
    let mut cursor = char_index;
    if cursor >= chars.len() || chars[cursor] != '<' {
        return None;
    }
    cursor += 1;
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    let ns: String = chars[cursor..chars.len().min(cursor + ANTML_NAMESPACE.len())].iter().collect();
    if ns == ANTML_NAMESPACE {
        cursor += ANTML_NAMESPACE.len();
    }
    let word_start = cursor;
    while cursor < chars.len() && chars[cursor].is_alphabetic() {
        cursor += 1;
    }
    let word: String = chars[word_start..cursor].iter().collect();
    if word != "parameter" || !(cursor < chars.len() && is_ws(chars[cursor])) {
        return None;
    }
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    let name_kw_start = cursor;
    while cursor < chars.len() && chars[cursor].is_alphabetic() {
        cursor += 1;
    }
    let name_kw: String = chars[name_kw_start..cursor].iter().collect();
    if name_kw != "name" {
        return None;
    }
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    if !(cursor < chars.len() && chars[cursor] == '=') {
        return None;
    }
    cursor += 1;
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    let quote = *chars.get(cursor)?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    cursor += 1;
    let value_start = cursor;
    while cursor < chars.len() && chars[cursor] != quote {
        cursor += 1;
    }
    if cursor >= chars.len() {
        return None;
    }
    let name_raw: String = chars[value_start..cursor].iter().collect();
    cursor += 1;
    while cursor < chars.len() && is_ws(chars[cursor]) {
        cursor += 1;
    }
    if !(cursor < chars.len() && chars[cursor] == '>') {
        return None;
    }
    cursor += 1;
    Some(ParameterOpenTagMatch { index: char_index, length: cursor - char_index, name: decode_xml_entities(&name_raw) })
}

/// Forward search for a full parameter open tag (name attribute plus closing `>`),
/// matching senpi's `findTag(PARAMETER_OPEN_TAG, ...)` inside `findParameterBoundary`.
fn find_parameter_open_tag_from(text: &str, from_char: usize) -> Option<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = from_char;
    while i < chars.len() {
        if chars[i] == '<'
            && let Some(found) = find_parameter_open_tag_at(text, i)
        {
            return Some((found.index, found.length));
        }
        i += 1;
    }
    None
}

pub fn find_parameter_boundary(text: &str, from_char: usize) -> Option<ParameterBoundary> {
    let mut cursor = from_char;
    let mut nested_invoke_depth = 0i64;
    loop {
        let parameter_close = find_parameter_close_tag(text, cursor);
        let invoke_open = find_nested_invoke_open(text, cursor);
        let invoke_close = find_invoke_close_tag(text, cursor);

        let next_index = [parameter_close.map(|m| m.0), invoke_open.map(|m| m.0), invoke_close.map(|m| m.0)]
            .into_iter()
            .flatten()
            .min();

        let next_index = next_index?;

        if let Some((idx, len)) = invoke_open
            && idx == next_index
        {
            let next_parameter_close = find_parameter_close_tag(text, idx + len);
            let nested_parameter_open = find_parameter_open_tag_from(text, idx + len);
            let nested_invoke_close = find_invoke_close_tag(text, idx + len);
            let should_skip_as_self_closing = match nested_invoke_close {
                None => true,
                Some((close_idx, _)) => {
                    nested_parameter_open.is_none_or(|(idx, _)| idx > close_idx)
                        && next_parameter_close.is_some_and(|(pc_idx, _)| close_idx > pc_idx)
                }
            };
            if should_skip_as_self_closing {
                cursor = idx + len;
                continue;
            }
            nested_invoke_depth += 1;
            cursor = idx + len;
            continue;
        }

        if let Some((idx, len)) = invoke_close
            && idx == next_index
        {
            if nested_invoke_depth == 0 {
                return Some(ParameterBoundary::InvokeClose(TagMatch { index: idx, length: len }));
            }
            nested_invoke_depth -= 1;
            cursor = idx + len;
            continue;
        }

        if let Some((idx, len)) = parameter_close {
            if nested_invoke_depth == 0 {
                return Some(ParameterBoundary::ParameterClose(TagMatch { index: idx, length: len }));
            }
            cursor = idx + len;
        }
    }
}

pub fn scan_invoke_block(text: &str, opening_tag: &InvokeOpenTagMatch) -> Option<InvokeBlockMatch> {
    let chars: Vec<char> = text.chars().collect();
    let opening_char_index = byte_to_char_index(text, opening_tag.index);
    let opening_char_length = byte_to_char_index(text, opening_tag.index + opening_tag.length) - opening_char_index;
    let mut parameters: Vec<InvokeParameter> = Vec::new();
    let mut cursor = opening_char_index + opening_char_length;

    loop {
        if cursor > chars.len() {
            return None;
        }
        let invoke_close = find_invoke_close_tag(text, cursor)?;
        let parameter_markup = find_parameter_markup(text, cursor);
        let nested_invoke_open = find_nested_invoke_open(text, cursor);

        if let Some((n_idx, _)) = nested_invoke_open
            && n_idx < invoke_close.0
            && parameter_markup.map(|(p_idx, _)| n_idx < p_idx).unwrap_or(true)
        {
            return None;
        }

        if parameter_markup.map(|(p_idx, _)| invoke_close.0 < p_idx).unwrap_or(true) {
            return Some(InvokeBlockMatch {
                content_end: char_index_to_byte(text, invoke_close.0),
                end: char_index_to_byte(text, invoke_close.0 + invoke_close.1),
                parameters: Some(parameters),
            });
        }

        let (p_idx, _) = parameter_markup.expect("the guard above returns when parameter_markup is None");
        let Some(parameter_open) = find_parameter_open_tag_at(text, p_idx) else {
            return Some(InvokeBlockMatch {
                content_end: char_index_to_byte(text, invoke_close.0),
                end: char_index_to_byte(text, invoke_close.0 + invoke_close.1),
                parameters: None,
            });
        };

        let value_start = parameter_open.index + parameter_open.length;
        let boundary = find_parameter_boundary(text, value_start)?;

        match boundary {
            ParameterBoundary::InvokeClose(m) => {
                return Some(InvokeBlockMatch {
                    content_end: char_index_to_byte(text, m.index),
                    end: char_index_to_byte(text, m.index + m.length),
                    parameters: None,
                });
            }
            ParameterBoundary::ParameterClose(m) => {
                let raw_value: String = chars[value_start..m.index].iter().collect();
                parameters.push(InvokeParameter { name: parameter_open.name, raw_value });
                cursor = m.index + m.length;
            }
        }
    }
}

fn is_attribute_prefix(remainder: &str) -> bool {
    let trimmed_leading: String = remainder.chars().skip_while(|c| c.is_whitespace()).collect();
    let attribute_name: String = trimmed_leading.chars().take_while(|c| c.is_alphabetic()).collect();
    let after_attribute_name = &trimmed_leading[attribute_name.len()..];

    if attribute_name.len() < "name".len() {
        return "name".starts_with(&attribute_name) && after_attribute_name.trim().is_empty();
    }
    if attribute_name != "name" {
        return false;
    }
    if after_attribute_name.trim().is_empty() {
        return true;
    }

    let after_ws: String = after_attribute_name.chars().skip_while(|c| c.is_whitespace()).collect();
    let Some(rest) = after_ws.strip_prefix('=') else { return false };
    let value_prefix: String = rest.chars().skip_while(|c| c.is_whitespace()).collect();
    if value_prefix.is_empty() {
        return true;
    }

    let quote = value_prefix.chars().next().expect("value_prefix is non-empty");
    if quote != '"' && quote != '\'' {
        return false;
    }

    match value_prefix[1..].find(quote) {
        None => true,
        Some(rel) => value_prefix[1 + rel + quote.len_utf8()..].trim().is_empty(),
    }
}

pub fn find_parameter_markup_pub(text: &str, from_index: usize) -> Option<TagMatch> {
    let from_char = byte_to_char_index(text, from_index);
    find_parameter_markup(text, from_char).map(|(idx, len)| TagMatch { index: char_index_to_byte(text, idx), length: len })
}

pub fn is_whitespace_or_invoke_close_prefix(remainder: &str) -> bool {
    let compact: String = remainder.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() {
        return true;
    }
    let prefix = "</invoke>";
    prefix.starts_with(&compact) && compact != prefix
}

pub fn is_potential_protocol_start(candidate: &str) -> bool {
    if !candidate.starts_with('<') {
        return false;
    }
    let body = &candidate[1..];
    let leading_ws_len = body.chars().take_while(|c| c.is_whitespace()).count();
    let leading_ws: String = body.chars().take(leading_ws_len).collect();
    let name_part = &body[leading_ws.len()..];
    if name_part.is_empty() || ANTML_NAMESPACE.starts_with(name_part) {
        return true;
    }

    for namespace in ["", ANTML_NAMESPACE] {
        if !name_part.starts_with(namespace) {
            continue;
        }
        let unqualified = &name_part[namespace.len()..];
        let tag_name = ["invoke", "parameter", "function_calls"]
            .into_iter()
            .find(|name| name.starts_with(unqualified) || unqualified.starts_with(name));
        let Some(tag_name) = tag_name else { continue };
        if unqualified.len() < tag_name.len() {
            return true;
        }

        let remainder = &unqualified[tag_name.len()..];
        if remainder.contains('>') {
            return false;
        }
        return if tag_name == "function_calls" { remainder.trim().is_empty() } else { is_attribute_prefix(remainder) };
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_invoke_open_tag_matches_double_and_single_quoted_names() {
        let m = find_invoke_open_tag(r#"<invoke name="get_weather">"#, 0).expect("match");
        assert_eq!(m.tool_name, "get_weather");
        assert_eq!(m.index, 0);
        assert_eq!(m.length, r#"<invoke name="get_weather">"#.len());

        let m2 = find_invoke_open_tag("<invoke name='x'>", 0).expect("match");
        assert_eq!(m2.tool_name, "x");
    }

    #[test]
    fn find_invoke_open_tag_returns_none_without_open_tag() {
        assert!(find_invoke_open_tag("no tags here", 0).is_none());
    }

    #[test]
    fn scan_invoke_block_collects_parameters_until_close() {
        let text = r#"<invoke name="t"><parameter name="a">1</parameter><parameter name="b">2</parameter></invoke>"#;
        let opening = find_invoke_open_tag(text, 0).expect("open");
        let block = scan_invoke_block(text, &opening).expect("block");
        let params = block.parameters.expect("params");
        assert_eq!(params.len(), 2);
        assert_eq!(params[0].name, "a");
        assert_eq!(params[0].raw_value, "1");
        assert_eq!(params[1].name, "b");
        assert_eq!(params[1].raw_value, "2");
    }

    #[test]
    fn scan_invoke_block_returns_none_without_close_tag() {
        let text = r#"<invoke name="t"><parameter name="a">1</parameter>"#;
        let opening = find_invoke_open_tag(text, 0).expect("open");
        assert!(scan_invoke_block(text, &opening).is_none());
    }

    #[test]
    fn find_incomplete_invoke_open_tag_accepts_a_truncated_open_tag() {
        let m = find_incomplete_invoke_open_tag(r#"<invoke name="get_weat"#, 0).expect("incomplete");
        assert_eq!(m.tool_name, "get_weat");
    }

    #[test]
    fn is_whitespace_or_invoke_close_prefix_true_for_partial_close_tag() {
        assert!(is_whitespace_or_invoke_close_prefix("  "));
        assert!(is_whitespace_or_invoke_close_prefix("</inv"));
        assert!(!is_whitespace_or_invoke_close_prefix("</invoke>"));
        assert!(!is_whitespace_or_invoke_close_prefix("xyz"));
    }

    #[test]
    fn is_potential_protocol_start_detects_open_angle_bracket_prefixes() {
        assert!(is_potential_protocol_start("<"));
        assert!(is_potential_protocol_start("<inv"));
        assert!(is_potential_protocol_start("<invoke name=\"x\""));
        assert!(!is_potential_protocol_start("<div>"));
        assert!(!is_potential_protocol_start("hello"));
    }
}
