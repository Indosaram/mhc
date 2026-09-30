//! Port of senpi packages/ai/src/auth/oauth/oauth-page.ts.

const LOGO_SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 800 800" aria-hidden="true"><path fill="#fff" fill-rule="evenodd" d="M165.29 165.29 H517.36 V400 H400 V517.36 H282.65 V634.72 H165.29 Z M282.65 282.65 V400 H400 V282.65 Z"/><path fill="#fff" d="M517.36 400 H634.72 V634.72 H517.36 Z"/></svg>"##;

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

const PAGE_TEMPLATE: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8" />
  <meta name="viewport" content="width=device-width, initial-scale=1" />
  <title>__MAHO_TITLE__</title>
  <style>
    :root {
      --text: #fafafa;
      --text-dim: #a1a1aa;
      --page-bg: #09090b;
      --font-sans: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "Helvetica Neue", Arial, "Noto Sans", sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji";
      --font-mono: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace;
    }
    * { box-sizing: border-box; }
    html { color-scheme: dark; }
    body {
      margin: 0;
      min-height: 100vh;
      display: flex;
      align-items: center;
      justify-content: center;
      padding: 24px;
      background: var(--page-bg);
      color: var(--text);
      font-family: var(--font-sans);
      text-align: center;
    }
    main {
      width: 100%;
      max-width: 560px;
      display: flex;
      flex-direction: column;
      align-items: center;
      justify-content: center;
    }
    .logo {
      width: 72px;
      height: 72px;
      display: block;
      margin-bottom: 24px;
    }
    h1 {
      margin: 0 0 10px;
      font-size: 28px;
      line-height: 1.15;
      font-weight: 650;
      color: var(--text);
    }
    p {
      margin: 0;
      line-height: 1.7;
      color: var(--text-dim);
      font-size: 15px;
    }
    .details {
      margin-top: 16px;
      font-family: var(--font-mono);
      font-size: 13px;
      color: var(--text-dim);
      white-space: pre-wrap;
      word-break: break-word;
    }
  </style>
</head>
<body>
  <main>
    <div class="logo">__MAHO_LOGO__</div>
    <h1>__MAHO_HEADING__</h1>
    <p>__MAHO_MESSAGE__</p>
    __MAHO_DETAILS__
  </main>
</body>
</html>"#;

fn render_page(title: &str, heading: &str, message: &str, details: Option<&str>) -> String {
    let details_block = details.map(|d| format!("<div class=\"details\">{}</div>", escape_html(d))).unwrap_or_default();
    PAGE_TEMPLATE
        .replace("__MAHO_TITLE__", &escape_html(title))
        .replace("__MAHO_HEADING__", &escape_html(heading))
        .replace("__MAHO_MESSAGE__", &escape_html(message))
        .replace("__MAHO_LOGO__", LOGO_SVG)
        .replace("__MAHO_DETAILS__", &details_block)
}

pub fn oauth_success_html(message: &str) -> String {
    render_page("Authentication successful", "Authentication successful", message, None)
}

pub fn oauth_error_html(message: &str, details: Option<&str>) -> String {
    render_page("Authentication failed", "Authentication failed", message, details)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_page_uses_the_success_titles_and_the_escaped_message() {
        let html = oauth_success_html("Anthropic authentication completed. You can close this window.");
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("<title>Authentication successful</title>"));
        assert!(html.contains("<h1>Authentication successful</h1>"));
        assert!(html.contains("<p>Anthropic authentication completed. You can close this window.</p>"));
        assert!(!html.contains("class=\"details\""));
        assert!(html.ends_with("</html>"));
    }

    #[test]
    fn error_page_renders_escaped_details_only_when_present() {
        let without = oauth_error_html("Callback route not found.", None);
        assert!(without.contains("<title>Authentication failed</title>"));
        assert!(!without.contains("class=\"details\""));

        let with = oauth_error_html("Failed <now>", Some("a & b"));
        assert!(with.contains("<p>Failed &lt;now&gt;</p>"));
        assert!(with.contains("<div class=\"details\">a &amp; b</div>"));
    }
}
