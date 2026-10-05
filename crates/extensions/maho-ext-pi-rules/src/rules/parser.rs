use super::types::{ParsedRule, PatternList, RuleFrontmatter};
pub(crate) fn js_whitespace(ch:char)->bool{matches!(ch,'\u{0009}'..='\u{000d}'|' '|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')}

pub fn parse_rule(content: &str) -> ParsedRule {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let opening = if content.starts_with("---\r\n") { 5 } else if content.starts_with("---\n") { 4 } else { 0 };
    let plain = |diagnostic| ParsedRule { frontmatter: RuleFrontmatter::default(), body: content.into(), diagnostic };
    if opening == 0 { return plain(None); }
    let mut start = opening;
    loop {
        let end = content[start..].find('\n').map_or(content.len(), |n| start + n);
        if content[start..end].strip_suffix('\r').unwrap_or(&content[start..end]) == "---" {
            let body_start = if end == content.len() { end } else { end + 1 };
            return match parse_yaml(&content[opening..start]) {
                Ok(frontmatter) => ParsedRule { frontmatter, body: content[body_start..].into(), diagnostic: None },
                Err(message) => plain(Some(format!("Malformed frontmatter: {message}"))),
            };
        }
        if end == content.len() { break; }
        start = end + 1;
    }
    plain(Some("Missing closing frontmatter delimiter".into()))
}

fn parse_yaml(yaml: &str) -> Result<RuleFrontmatter, String> {
    let normalized = yaml.replace("\r\n", "\n");
    let lines: Vec<_> = normalized.split('\n').collect();
    let mut result = RuleFrontmatter::default();
    let mut globs = Vec::new();
    let mut index = 0;
    while let Some(raw) = lines.get(index) {
        let line = strip_comment(raw).trim_matches(js_whitespace);
        if line.is_empty() { index += 1; continue; }
        let (key, value) = line.split_once(':').ok_or_else(|| format!("Expected key-value pair on line {}", index + 1))?;
        let value = value.trim_matches(js_whitespace);
        match key.trim_matches(js_whitespace) {
            "description" => result.description = Some(parse_string(value)?),
            "alwaysApply" => result.always_apply = Some(match value {
                "true" => true, "false" => false,
                _ => return Err(format!("Expected boolean on line {}", index + 1)),
            }),
            "globs" | "paths" | "applyTo" => {
                let values = if value.starts_with('[') {
                    parse_inline(value)?
                } else if value.is_empty() {
                    let mut values = Vec::new();
                    while let Some(raw_item) = lines.get(index + 1) {
                        let item = strip_comment(raw_item);
                        if item.trim_matches(js_whitespace).is_empty() { index += 1; continue; }
                        if !item.starts_with(js_whitespace) { break; }
                        let Some(item) = item.trim_start_matches(js_whitespace).strip_prefix('-') else { break; };
                        let parsed=parse_string(item.trim_start_matches(js_whitespace))?;if !parsed.is_empty(){values.push(parsed);}
                        index += 1;
                    }
                    values
                } else {
                    let parsed = parse_string(value)?;
                    if parsed.contains(',') { parsed.split(',').map(|item|item.trim_matches(js_whitespace)).filter(|item|!item.is_empty()).map(str::to_owned).collect() } else { vec![parsed] }
                };
                for glob in values { if !globs.contains(&glob) { globs.push(glob); } }
            }
            _ => {}
        }
        index += 1;
    }
    result.globs = match globs.len() {
        0 => None,
        1 => globs.pop().map(PatternList::Single),
        _ => Some(PatternList::Multiple(globs)),
    };
    Ok(result)
}

fn parse_string(value: &str) -> Result<String, String> {
    if value.starts_with('"') { return serde_json::from_str::<String>(value).map_err(|_| "Invalid JSON-quoted string".into()); }
    if value=="'" {return Ok(String::new());}
    if value.starts_with('\'') {
        return value.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')).map(str::to_owned).ok_or_else(|| "Unclosed quoted value".into());
    }
    Ok(value.into())
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in line.char_indices() {
        if escaped { escaped = false; continue; }
        if quote.is_some() && ch == '\\' { escaped = true; continue; }
        if ch == '\'' || ch == '"' {
            if quote == Some(ch) { quote = None; } else if quote.is_none() { quote = Some(ch); }
        } else if quote.is_none() && ch == '#' { return &line[..index]; }
    }
    line
}

fn parse_inline(value: &str) -> Result<Vec<String>, String> {
    let mut quote = None;
    let mut escaped = false;
    let mut start = 1;
    let mut values = Vec::new();
    for (index, ch) in value.char_indices().skip(1) {
        if escaped { escaped = false; continue; }
        if quote.is_some() && ch == '\\' { escaped = true; continue; }
        if ch == '\'' || ch == '"' {
            if quote == Some(ch) { quote = None; } else if quote.is_none() { quote = Some(ch); }
        } else if quote.is_none() && (ch == ',' || ch == ']') {
            let part = value[start..index].trim_matches(js_whitespace);
            if !part.is_empty() { let parsed = parse_string(part)?; if !parsed.is_empty() { values.push(parsed); } }
            start = index + 1;
            if ch == ']' {
                if !value[start..].trim_matches(js_whitespace).is_empty() { return Err("Unexpected content after inline array".into()); }
                return Ok(values);
            }
        }
    }
    Err("Unclosed inline array".into())
}
