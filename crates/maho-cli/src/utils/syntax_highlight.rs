use std::{collections::BTreeMap, sync::LazyLock};
pub type HighlightFormatter = Box<dyn Fn(&str) -> String>;
pub type HighlightTheme = BTreeMap<String, HighlightFormatter>;
fn formatter<'a>(scopes: &[Option<String>], theme: &'a HighlightTheme) -> Option<&'a HighlightFormatter> {
    for scope in scopes.iter().rev().flatten() {
        if let Some(formatter) = theme.get(scope) { return Some(formatter); }
        for separator in ['.', '-'] {
            if let Some((prefix, _)) = scope.split_once(separator) && let Some(formatter) = theme.get(prefix) { return Some(formatter); }
        }
    }
    theme.get("default")
}
fn flush(output: &mut String, buffer: &mut String, scopes: &[Option<String>], theme: &HighlightTheme) {
    if buffer.is_empty() { return; }
    if let Some(formatter) = formatter(scopes, theme) { output.push_str(&formatter(buffer)); } else { output.push_str(buffer); }
    buffer.clear();
}
pub fn render_highlighted_html(html: &str, theme: &HighlightTheme) -> String {
    static CLASS: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r#"\sclass\s*=\s*(?:"([^"]*)"|'([^']*)')"#).expect("highlight class regex"));
    let mut output = String::new();
    let mut buffer = String::new();
    let mut scopes = Vec::new();
    let mut index = 0;
    while index < html.len() {
        let tail = &html[index..];
        if tail.starts_with("<span") && tail.as_bytes().get(5).is_some_and(|byte| matches!(byte, b'>' | b' ' | b'\t' | b'\n' | b'\r')) && let Some(end) = tail[5..].find('>') {
            flush(&mut output, &mut buffer, &scopes, theme);
            let end = end + 5;
            let scope = CLASS.captures(&tail[..=end]).and_then(|captures| captures.get(1).or_else(|| captures.get(2))).and_then(|class| class.as_str().split_whitespace().find_map(|name| name.strip_prefix("hljs-"))).map(str::to_owned);
            scopes.push(scope);
            index += end + 1;
            continue;
        }
        if tail.starts_with("</span>") {
            flush(&mut output, &mut buffer, &scopes, theme);
            scopes.pop();
            index += 7;
            continue;
        }
        if tail.starts_with('&') && let Some(end) = tail.find(';') && tail[..end].encode_utf16().count() <= 16 && let Some(decoded) = super::html::decode_html_entity(&tail[1..end]) {
            buffer.push_str(&decoded);
            index += end + 1;
            continue;
        }
        let character = tail.chars().next().expect("nonempty HTML tail");
        buffer.push(character);
        index += character.len_utf8();
    }
    flush(&mut output, &mut buffer, &scopes, theme);
    output
}
