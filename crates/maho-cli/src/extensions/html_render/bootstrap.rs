//! Port of `packages/coding-agent/src/extensions/html-render/bootstrap.ts`.

use std::sync::LazyLock;

use regex::Regex;

const BASE_CSS: &str = "html{background:var(--background);color:var(--foreground);font-family:var(--font-sans);font-size:14px;line-height:1.5;-webkit-font-smoothing:antialiased;-webkit-text-size-adjust:100%;scrollbar-width:none}html::-webkit-scrollbar{display:none}body{margin:0}code,kbd,pre,samp{font-family:var(--font-mono)}";

const DARK_VARS: [(&str, &str); 7] = [
    ("--background", "#0a0a0a"),
    ("--foreground", "#f5f5f5"),
    ("--muted-foreground", "#818181"),
    ("--chart-1", "#60a5fa"),
    (
        "--font-sans",
        r#"-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif"#,
    ),
    (
        "--font-mono",
        r#""SF Mono", "SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace"#,
    ),
    ("--radius", "0.625rem"),
];

const LIGHT_VARS: [(&str, &str); 7] = [
    ("--background", "#ffffff"),
    ("--foreground", "#0a0a0a"),
    ("--muted-foreground", "#525252"),
    ("--chart-1", "#1d4ed8"),
    (
        "--font-sans",
        r#"-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif"#,
    ),
    (
        "--font-mono",
        r#""SF Mono", "SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace"#,
    ),
    ("--radius", "0.625rem"),
];

fn root_rule(appearance: &str, variables: &[(&str, &str)]) -> String {
    let mut out = format!(":root{{color-scheme:{appearance};");
    for (name, value) in variables {
        out.push_str(name);
        out.push(':');
        out.push_str(value);
        out.push(';');
    }
    out.push('}');
    out
}

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

static META_CHARSET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<meta\s[^>]*charset").expect("charset regex"));
static META_VIEWPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)<meta\s[^>]*name\s*=\s*["']?viewport"#).expect("viewport regex")
});
static TEMPLATE_TAG: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<(/?)template(?:\s[^>]*)?/?>").expect("template regex"));
static HEAD_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<head(?:\s[^>]*)?>").expect("head regex"));
static HTML_OPEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)<html(?:\s[^>]*)?>").expect("html regex"));
static DOC_DOCTYPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^\s*<!doctype[^>]*>").expect("doctype regex"));
static LEADING_ASCII_DOCTYPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\u{feff}?[\t\n\f\r ]*<!doctype[ -=?-~]*>").expect("ascii doctype regex")
});

fn bootstrap_script() -> String {
    let base = serde_json::to_string(BASE_CSS).unwrap_or_default();
    let light = serde_json::to_string(&root_rule("light", &LIGHT_VARS)).unwrap_or_default();
    let dark = serde_json::to_string(&root_rule("dark", &DARK_VARS)).unwrap_or_default();
    format!(
        r#"(function(){{var s=document.getElementById("t3-theme");if(!s)return;var b={base};try{{var m=window.matchMedia("(prefers-color-scheme: light)");var a=function(){{s.textContent=m.matches?{light}:{dark}+b;}};a();if(m.addEventListener)m.addEventListener("change",a);}}catch(e){{}}}})();"#
    )
}

fn bootstrap_markup(scan: &str) -> String {
    let head_prefix: String = scan.chars().take(4096).collect();
    let charset = if META_CHARSET.is_match(&head_prefix) {
        ""
    } else {
        r#"<meta charset="utf-8">"#
    };
    let viewport = if META_VIEWPORT.is_match(scan) {
        ""
    } else {
        r#"<meta name="viewport" content="width=device-width, initial-scale=1">"#
    };
    let default_css = format!(
        "{}@media (prefers-color-scheme: light){{{}}}{}",
        root_rule("dark", &DARK_VARS),
        root_rule("light", &LIGHT_VARS),
        BASE_CSS
    );
    format!(
        r#"{charset}{viewport}<style id="t3-theme">{default_css}</style><script>{}</script>"#,
        bootstrap_script()
    )
}

fn blank_non_markup(html: &str) -> String {
    // Blank comments, raw-text elements, and <plaintext> tails with spaces while preserving byte indices.
    let lower = html.to_ascii_lowercase();
    let mut buf: Vec<u8> = html.as_bytes().to_vec();
    let mut at = 0usize;
    const RAW: [&str; 9] = [
        "script", "style", "textarea", "title", "xmp", "iframe", "noembed", "noframes", "noscript",
    ];
    while at < html.len() {
        let Some(rel) = html[at..].find('<') else {
            break;
        };
        let open = at + rel;
        if html[open..].starts_with("<!--") {
            let end = html[open + 4..]
                .find("-->")
                .map_or(html.len(), |pos| open + 4 + pos + 3);
            for b in &mut buf[open..end] {
                *b = b' ';
            }
            at = end;
            continue;
        }
        if lower[open..].starts_with("<plaintext")
            && html
                .as_bytes()
                .get(open + 10)
                .is_none_or(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'>' | b'/'))
        {
            for b in &mut buf[open..] {
                *b = b' ';
            }
            break;
        }
        let mut matched_raw = None;
        for tag in RAW {
            let prefix_len = 1 + tag.len();
            if lower[open..].starts_with(&format!("<{tag}"))
                && html.as_bytes().get(open + prefix_len).is_some_and(|b| {
                    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' | b'>' | b'/')
                })
            {
                matched_raw = Some(tag);
                break;
            }
        }
        if let Some(tag) = matched_raw {
            let close_token = format!("</{tag}");
            let mut search = open + 1 + tag.len();
            let mut end = html.len();
            while let Some(pos) = lower[search..].find(&close_token) {
                let abs = search + pos;
                let after = abs + close_token.len();
                let tail = html[after..].trim_start_matches([' ', '\t', '\n', '\r']);
                if tail.starts_with('>') {
                    let consumed = html.len() - tail.len() + 1;
                    end = consumed;
                    break;
                }
                search = after;
            }
            for b in &mut buf[open..end] {
                *b = b' ';
            }
            at = end;
            continue;
        }
        at = open + 1;
    }
    let scan = String::from_utf8(buf).unwrap_or_else(|_| html.to_owned());
    let mut parts = String::with_capacity(scan.len());
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut cursor = 0usize;
    for m in TEMPLATE_TAG.captures_iter(&scan) {
        let Some(full) = m.get(0) else {
            continue;
        };
        let is_close = m.get(1).is_some_and(|g| !g.as_str().is_empty());
        if !is_close {
            if depth == 0 {
                start = full.start();
            }
            depth += 1;
        } else if depth > 0 {
            depth -= 1;
            if depth == 0 {
                parts.push_str(&scan[cursor..start]);
                parts.push_str(&" ".repeat(full.end() - start));
                cursor = full.end();
            }
        }
    }
    if depth > 0 {
        parts.push_str(&scan[cursor..start]);
        parts.push_str(&" ".repeat(scan.len() - start));
        cursor = scan.len();
    }
    parts.push_str(&scan[cursor..]);
    parts
}

fn inject_theme_bootstrap(html: &str) -> String {
    let scan = blank_non_markup(html);
    let markup = bootstrap_markup(&scan);
    if let Some(m) = HEAD_OPEN.find(&scan) {
        let at = m.end();
        return format!("{}{markup}{}", &html[..at], &html[at..]);
    }
    if let Some(m) = HTML_OPEN.find(&scan) {
        let at = m.end();
        return format!("{}<head>{markup}</head>{}", &html[..at], &html[at..]);
    }
    if let Some(m) = DOC_DOCTYPE.find(html) {
        let at = m.end();
        return format!("{}<head>{markup}</head>{}", &html[..at], &html[at..]);
    }
    format!("<!doctype html><head>{markup}</head>{html}")
}

pub fn inject_html_render_bootstrap(html: &str) -> String {
    let policy_meta = format!(
        r#"<meta http-equiv="Content-Security-Policy" content="{}">"#,
        html_render_content_security_policy()
    );
    let preamble = format!("\u{feff}<!doctype html>{policy_meta}");
    let themed = inject_theme_bootstrap(html);
    let stripped = LEADING_ASCII_DOCTYPE.replace(&themed, "");
    format!("{preamble}{stripped}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injects_bom_doctype_csp_and_theme_inside_head() {
        let out = inject_html_render_bootstrap(
            "<!DOCTYPE html><html><head><title>X</title></head><body>hi</body></html>",
        );
        assert!(
            out.starts_with("\u{feff}<!doctype html><meta http-equiv=\"Content-Security-Policy\"")
        );
        assert!(out.contains("default-src 'none'"));
        assert!(out.contains(r#"<style id="t3-theme">"#));
        assert!(out.contains("<title>X</title>"));
    }

    #[test]
    fn ignores_head_inside_template_or_comment() {
        let out = inject_html_render_bootstrap(
            "<!-- <head> --><template><head></head></template><p>ok</p>",
        );
        let idx_head = out.find("<style id=\"t3-theme\">").expect("theme style");
        let idx_tpl = out.find("<template>").expect("template");
        assert!(idx_head < idx_tpl);
    }
}
