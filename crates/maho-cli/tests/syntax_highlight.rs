use maho_cli::utils::syntax_highlight::*;
#[test]
fn upstream_renderer_formats_keyword_scope() {
    let theme = HighlightTheme::from([("keyword".to_owned(), Box::new(|text: &str| format!("[keyword:{text}]")) as HighlightFormatter)]);
    assert_eq!(render_highlighted_html("<span class=\"hljs-keyword\">const</span> value", &theme), "[keyword:const] value");
}
#[test]
fn upstream_renderer_decodes_emitted_html_entities() {
    assert_eq!(render_highlighted_html("&lt;tag attr=&quot;value&quot;&gt;&amp;#x41;&#65;&lt;/tag&gt;", &HighlightTheme::new()), "<tag attr=\"value\">&#x41;A</tag>");
}
#[test]
fn upstream_renderer_inherits_parent_for_unmapped_scope() {
    let theme = HighlightTheme::from([("string".to_owned(), Box::new(|text: &str| format!("[string:{text}]")) as HighlightFormatter)]);
    assert_eq!(render_highlighted_html("<span class=\"hljs-string\">a<span class=\"hljs-subst\">${x}</span>b</span>", &theme), "[string:a][string:${x}][string:b]");
}
#[test]
fn upstream_renderer_keeps_parent_across_unscoped_span() {
    let theme = HighlightTheme::from([("string".to_owned(), Box::new(|text: &str| format!("[string:{text}]")) as HighlightFormatter)]);
    assert_eq!(render_highlighted_html("<span class=\"hljs-string\">a<span class=\"language-xml\">b</span>c</span>", &theme), "[string:a][string:b][string:c]");
}
