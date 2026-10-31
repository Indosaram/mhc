//! The theme bootstrap injection and base stylesheet, mirroring the desktop's
//! packages/shared/src/htmlRender.ts (upstream t3code #15968). Pure string functions.
use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use std::sync::OnceLock;

const BASE_CSS: &str = "html{background:var(--background);color:var(--foreground);font-family:var(--font-sans);font-size:14px;line-height:1.5;-webkit-font-smoothing:antialiased;-webkit-text-size-adjust:100%;scrollbar-width:none}html::-webkit-scrollbar{display:none}body{margin:0}code,kbd,pre,samp{font-family:var(--font-mono)}";

const DARK_VARS: [(&str, &str); 7] = [
    ("--background", "#0a0a0a"),
    ("--foreground", "#f5f5f5"),
    ("--muted-foreground", "#818181"),
    ("--chart-1", "#60a5fa"),
    (
        "--font-sans",
        "-apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif",
    ),
    (
        "--font-mono",
        "\"SF Mono\", \"SFMono-Regular\", Menlo, Consolas, \"Liberation Mono\", monospace",
    ),
    ("--radius", "0.625rem"),
];

/// Dark variables with the light overrides applied in place (`{...DARK_VARS, ...overrides}`).
fn light_vars() -> Vec<(&'static str, &'static str)> {
    let overrides = [
        ("--background", "#ffffff"),
        ("--foreground", "#0a0a0a"),
        ("--muted-foreground", "#525252"),
        ("--chart-1", "#1d4ed8"),
    ];
    DARK_VARS
        .iter()
        .map(|(name, value)| {
            (
                *name,
                overrides
                    .iter()
                    .find(|(key, _)| key == name)
                    .map_or(*value, |(_, over)| *over),
            )
        })
        .collect()
}

fn root_rule(appearance: &str, variables: &[(&str, &str)]) -> String {
    let declarations = variables
        .iter()
        .map(|(name, value)| format!("{name}:{value};"))
        .collect::<String>();
    format!(":root{{color-scheme:{appearance};{declarations}}}")
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

fn bootstrap_script() -> String {
    format!(
        "(function(){{var s=document.getElementById(\"t3-theme\");if(!s)return;var b={};try{{var m=window.matchMedia(\"(prefers-color-scheme: light)\");var a=function(){{s.textContent=m.matches?{}:{}+b;}};a();if(m.addEventListener)m.addEventListener(\"change\",a);}}catch(e){{}}}})();",
        json_string(BASE_CSS),
        json_string(&root_rule("light", &light_vars())),
        json_string(&root_rule("dark", &DARK_VARS)),
    )
}

fn static_regex(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| {
        Regex::new(pattern)
            .unwrap_or_else(|error| panic!("invalid static pattern {pattern}: {error}"))
    })
}

/// Largest char boundary at or below `limit`.
fn floor_boundary(text: &str, limit: usize) -> usize {
    let mut at = limit.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn bootstrap_markup(markup: &str) -> String {
    static CHARSET: OnceLock<Regex> = OnceLock::new();
    static VIEWPORT: OnceLock<Regex> = OnceLock::new();
    let default_css = format!(
        "{}@media (prefers-color-scheme: light){{{}}}{BASE_CSS}",
        root_rule("dark", &DARK_VARS),
        root_rule("light", &light_vars())
    );
    let head = &markup[..floor_boundary(markup, 4096)];
    [
        if static_regex(&CHARSET, r"(?i)<meta\s[^>]*charset").is_match(head) { "" } else { "<meta charset=\"utf-8\">" }.to_owned(),
        if static_regex(&VIEWPORT, r#"(?i)<meta\s[^>]*name\s*=\s*["']?viewport"#).is_match(markup) {
            ""
        } else {
            "<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">"
        }
        .to_owned(),
        format!("<style id=\"t3-theme\">{default_css}</style>"),
        format!("<script>{}</script>", bootstrap_script()),
    ]
    .concat()
}

fn blank(length: usize) -> String {
    " ".repeat(length)
}

/// The markup with comments, raw-text elements and `<template>` contents blanked to spaces
/// (same byte length), so a tag-looking string inside them is never mistaken for the document's own.
fn blank_non_markup(html: &str) -> String {
    static NON_MARKUP: OnceLock<FancyRegex> = OnceLock::new();
    static TEMPLATE: OnceLock<Regex> = OnceLock::new();
    let non_markup = NON_MARKUP.get_or_init(|| {
        FancyRegex::new(r"(?i)<!--[\s\S]*?(?:-->|$)|<(script|style|textarea|title|xmp|iframe|noembed|noframes|noscript)\b[\s\S]*?(?:</\1\s*>|$)|<plaintext\b[\s\S]*$")
            .unwrap_or_else(|error| panic!("invalid static pattern: {error}"))
    });
    let mut scan = String::with_capacity(html.len());
    let mut last = 0;
    for found in non_markup.find_iter(html).flatten() {
        scan.push_str(&html[last..found.start()]);
        scan.push_str(&blank(found.len()));
        last = found.end();
    }
    scan.push_str(&html[last..]);
    let template = static_regex(&TEMPLATE, r"(?i)<(/?)template(?:\s[^>]*)?/?>");
    let mut parts = String::with_capacity(scan.len());
    let (mut depth, mut start, mut at) = (0usize, 0usize, 0usize);
    for captures in template.captures_iter(&scan) {
        let whole = captures.get(0).map_or(0..0, |found| found.range());
        if captures
            .get(1)
            .is_none_or(|slash| slash.as_str().is_empty())
        {
            if depth == 0 {
                start = whole.start;
            }
            depth += 1;
        } else if depth > 0 {
            depth -= 1;
            if depth == 0 {
                parts.push_str(&scan[at..start]);
                parts.push_str(&blank(whole.end - start));
                at = whole.end;
            }
        }
    }
    if depth > 0 {
        parts.push_str(&scan[at..start]);
        parts.push_str(&blank(scan.len() - start));
        at = scan.len();
    }
    parts.push_str(&scan[at..]);
    parts
}

/// A page is a static snapshot: everything it shows is in the document (inline, data: or blob:), and it
/// reaches no network. Its scripts run whenever it opens, but fetch, XHR, WebSocket, EventSource, remote
/// scripts, styles, images and fonts, frames, form posts and <base> are refused. The policy is the
/// document's first element, so nothing the page writes loads before it.
pub fn html_render_content_security_policy() -> String {
    [
        "default-src 'none'",
        "script-src 'unsafe-inline' 'unsafe-eval' data: blob:",
        "style-src 'unsafe-inline' data:",
        "img-src data: blob:",
        "font-src data:",
        "media-src data: blob:",
        "worker-src blob:",
        "connect-src 'none'",
        "frame-src 'none'",
        "form-action 'none'",
        "base-uri 'none'",
    ]
    .join("; ")
}

/// Every written page starts with a UTF-8 byte order mark, a standards-mode doctype and the policy, in
/// that order and in plain ASCII. The mark makes the browser decode the file as UTF-8 whatever charset the
/// page declares, so no declared encoding can turn the policy into text, and nothing the page wrote comes
/// before it. A leading doctype of the page's own is dropped only when it is printable ASCII; anything
/// else stays where it is, after the policy, where the parser ignores a doctype.
fn preamble() -> String {
    format!(
        "\u{FEFF}<!doctype html><meta http-equiv=\"Content-Security-Policy\" content=\"{}\">",
        html_render_content_security_policy()
    )
}

/// Inserts the theme bootstrap at the start of the document head, and the snapshot policy ahead of
/// everything the page wrote.
pub fn inject_html_render_bootstrap(html: &str) -> String {
    static LEADING_DOCTYPE: OnceLock<Regex> = OnceLock::new();
    let themed = inject_theme_bootstrap(html);
    let leading = static_regex(
        &LEADING_DOCTYPE,
        r"(?i)^\x{FEFF}?[\t\n\x0C\r ]*<!doctype[ -=?-~]*>",
    );
    let rest = leading
        .find(&themed)
        .map_or(themed.as_str(), |found| &themed[found.end()..]);
    preamble() + rest
}

fn inject_theme_bootstrap(html: &str) -> String {
    static HEAD_OPEN: OnceLock<Regex> = OnceLock::new();
    static HTML_OPEN: OnceLock<Regex> = OnceLock::new();
    static DOCTYPE: OnceLock<Regex> = OnceLock::new();
    let scan = blank_non_markup(html);
    let markup = bootstrap_markup(&scan);
    if let Some(open) = static_regex(&HEAD_OPEN, r"(?i)<head(?:\s[^>]*)?>").find(&scan) {
        return format!("{}{markup}{}", &html[..open.end()], &html[open.end()..]);
    }
    if let Some(open) = static_regex(&HTML_OPEN, r"(?i)<html(?:\s[^>]*)?>").find(&scan) {
        return format!(
            "{}<head>{markup}</head>{}",
            &html[..open.end()],
            &html[open.end()..]
        );
    }
    if let Some(doctype) = static_regex(&DOCTYPE, r"(?i)^\s*<!doctype[^>]*>").find(html) {
        return format!(
            "{}<head>{markup}</head>{}",
            &html[..doctype.end()],
            &html[doctype.end()..]
        );
    }
    format!("<!doctype html><head>{markup}</head>{html}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_and_bom_come_before_everything_the_page_wrote() {
        let out = inject_html_render_bootstrap(
            "<!DOCTYPE html><html><head><title>x</title></head><body>hi</body></html>",
        );
        assert!(
            out.starts_with("\u{FEFF}<!doctype html><meta http-equiv=\"Content-Security-Policy\"")
        );
        assert_eq!(
            out.matches("<!doctype html>").count() + out.matches("<!DOCTYPE html>").count(),
            1
        );
        assert!(out.contains("<head><meta charset=\"utf-8\">"));
        assert!(out.contains("<style id=\"t3-theme\">"));
    }

    #[test]
    fn head_inside_a_comment_or_script_is_not_the_injection_point() {
        let out = inject_html_render_bootstrap(
            "<!-- <head> --><script>var a='<head>';</script><head></head>",
        );
        assert!(out.contains("</script><head><meta charset=\"utf-8\">"));
        assert!(out.contains("t3-theme\""));
    }

    #[test]
    fn fragments_without_html_get_a_head() {
        let out = inject_html_render_bootstrap("<p>hello</p>");
        assert!(out.contains("<head><meta charset=\"utf-8\">"));
        assert!(out.ends_with("</head><p>hello</p>"));
    }
}
