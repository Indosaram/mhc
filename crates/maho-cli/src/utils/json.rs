pub fn strip_json_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len()); let mut chars = input.chars().peekable(); let mut quoted = false; let mut escaped = false;
    while let Some(c) = chars.next() {
        if quoted { out.push(c); if escaped { escaped = false; } else if c == '\\' { escaped = true; } else if c == '"' { quoted = false; } }
        else if c == '"' { quoted = true; out.push(c); }
        else if c == '/' && chars.peek() == Some(&'/') { chars.next(); while chars.peek().is_some_and(|c| *c != '\n') { chars.next(); } }
        else { out.push(c); }
    }
    let mut result = String::with_capacity(out.len()); let mut quoted = false; let mut escaped = false; let mut chars = out.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted { result.push(c); if escaped { escaped = false; } else if c == '\\' { escaped = true; } else if c == '"' { quoted = false; } }
        else if c == '"' { quoted = true; result.push(c); }
        else if c == ',' { let mut lookahead = chars.clone(); while lookahead.peek().is_some_and(|c| c.is_whitespace()) { lookahead.next(); } if !matches!(lookahead.peek(), Some('}' | ']')) { result.push(c); } }
        else { result.push(c); }
    } result
}
