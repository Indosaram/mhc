//! Escaping for values embedded in a double-quoted shell command.

pub fn shell_escape_for_double_quoted_command(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if matches!(
            ch,
            '\\' | '$' | '`' | '"' | ';' | '|' | '&' | '#' | '(' | ')'
        ) {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}
