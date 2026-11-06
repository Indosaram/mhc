use std::{collections::BTreeMap, sync::LazyLock};

pub const EAGER_HIGHLIGHT_LANGUAGES: &[&str] = &[
    "python",
    "java",
    "go",
    "javascript",
    "json",
    "cpp",
    "typescript",
    "php",
    "ruby",
    "c",
    "csharp",
    "nix",
    "bash",
    "rust",
    "scala",
    "kotlin",
    "swift",
    "dart",
    "groovy",
    "perl",
    "lua",
];

/// highlight.js aliases of the eager languages (`hljs.getLanguage` resolves names and aliases,
/// case-insensitively).
const EAGER_HIGHLIGHT_ALIASES: &[(&str, &[&str])] = &[
    ("python", &["py", "gyp", "ipython"]),
    ("java", &["jsp"]),
    ("go", &["golang"]),
    ("javascript", &["js", "jsx", "mjs", "cjs"]),
    ("json", &["jsonc", "json5"]),
    ("cpp", &["cc", "c++", "h++", "hpp", "hh", "hxx", "cxx"]),
    ("typescript", &["ts", "tsx", "mts", "cts"]),
    ("php", &[]),
    ("ruby", &["rb", "gemspec", "podspec", "thor", "irb"]),
    ("c", &["h"]),
    ("csharp", &["cs", "c#"]),
    ("nix", &["nixos"]),
    ("bash", &["sh", "zsh"]),
    ("rust", &["rs"]),
    ("scala", &[]),
    ("kotlin", &["kt", "kts", "ktm", "ktx"]),
    ("swift", &[]),
    ("dart", &[]),
    ("groovy", &[]),
    ("perl", &["pl", "pm"]),
    ("lua", &["pluto"]),
];

/// senpi `supportsLanguage`: only the eagerly registered languages answer at startup. The deferred
/// full highlight.js bundle (`loadAllHighlightLanguages`) has no native counterpart: code blocks are
/// coloured by `maho_interactive::theme::highlight::highlight_code` (syntect grammars).
pub fn supports_language(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    EAGER_HIGHLIGHT_ALIASES
        .iter()
        .any(|(language, aliases)| *language == name || aliases.contains(&name.as_str()))
}

pub type HighlightFormatter = Box<dyn Fn(&str) -> String>;
pub type HighlightTheme = BTreeMap<String, HighlightFormatter>;
fn formatter<'a>(
    scopes: &[Option<String>],
    theme: &'a HighlightTheme,
) -> Option<&'a HighlightFormatter> {
    for scope in scopes.iter().rev().flatten() {
        if let Some(formatter) = theme.get(scope) {
            return Some(formatter);
        }
        for separator in ['.', '-'] {
            if let Some((prefix, _)) = scope.split_once(separator)
                && let Some(formatter) = theme.get(prefix)
            {
                return Some(formatter);
            }
        }
    }
    theme.get("default")
}
fn flush(
    output: &mut String,
    buffer: &mut String,
    scopes: &[Option<String>],
    theme: &HighlightTheme,
) {
    if buffer.is_empty() {
        return;
    }
    if let Some(formatter) = formatter(scopes, theme) {
        output.push_str(&formatter(buffer));
    } else {
        output.push_str(buffer);
    }
    buffer.clear();
}
pub fn render_highlighted_html(html: &str, theme: &HighlightTheme) -> String {
    static CLASS: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r#"\sclass\s*=\s*(?:"([^"]*)"|'([^']*)')"#)
            .expect("highlight class regex")
    });
    let mut output = String::new();
    let mut buffer = String::new();
    let mut scopes = Vec::new();
    let mut index = 0;
    while index < html.len() {
        let tail = &html[index..];
        if tail.starts_with("<span")
            && tail
                .as_bytes()
                .get(5)
                .is_some_and(|byte| matches!(byte, b'>' | b' ' | b'\t' | b'\n' | b'\r'))
            && let Some(end) = tail[5..].find('>')
        {
            flush(&mut output, &mut buffer, &scopes, theme);
            let end = end + 5;
            let scope = CLASS
                .captures(&tail[..=end])
                .and_then(|captures| captures.get(1).or_else(|| captures.get(2)))
                .and_then(|class| {
                    class
                        .as_str()
                        .split_whitespace()
                        .find_map(|name| name.strip_prefix("hljs-"))
                })
                .map(str::to_owned);
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
        if tail.starts_with('&')
            && let Some(end) = tail.find(';')
            && tail[..end].encode_utf16().count() <= 16
            && let Some(decoded) = super::html::decode_html_entity(&tail[1..end])
        {
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

/// senpi `highlight(code, { language, ignoreIllegals, theme })`: `code` tokenized into
/// highlight.js classes (by the TUI's grammar set, `maho_interactive::theme::highlight`) and each
/// run passed through the `theme` formatter of its class, as `renderHighlightedHtml` does for
/// highlight.js output. `Err` when the language is not registered (`hljs.highlight` throws). The
/// no-language branch (`hljs.highlightAuto`) has no counterpart: the only caller, `highlightCode`,
/// always passes a supported language.
pub fn highlight(code: &str, language: &str, theme: &HighlightTheme) -> Result<String, String> {
    let runs = maho_interactive::theme::highlight::highlight_runs(code, language)?;
    let mut output = String::new();
    for (class, text) in runs {
        match formatter(&[class.map(str::to_owned)], theme) {
            Some(format) => output.push_str(&format(&text)),
            None => output.push_str(&text),
        }
    }
    Ok(output)
}
