use maho_cli::utils::syntax_highlight::*;

#[test]
fn json_is_registered_as_an_eager_highlight_language() {
    assert!(EAGER_HIGHLIGHT_LANGUAGES.contains(&"json"));
}

#[test]
fn upstream_renderer_formats_keyword_scope() {
    let theme = HighlightTheme::from([(
        "keyword".to_owned(),
        Box::new(|text: &str| format!("[keyword:{text}]")) as HighlightFormatter,
    )]);
    assert_eq!(
        render_highlighted_html("<span class=\"hljs-keyword\">const</span> value", &theme),
        "[keyword:const] value"
    );
}
#[test]
fn upstream_renderer_decodes_emitted_html_entities() {
    assert_eq!(
        render_highlighted_html(
            "&lt;tag attr=&quot;value&quot;&gt;&amp;#x41;&#65;&lt;/tag&gt;",
            &HighlightTheme::new()
        ),
        "<tag attr=\"value\">&#x41;A</tag>"
    );
}
#[test]
fn upstream_renderer_inherits_parent_for_unmapped_scope() {
    let theme = HighlightTheme::from([(
        "string".to_owned(),
        Box::new(|text: &str| format!("[string:{text}]")) as HighlightFormatter,
    )]);
    assert_eq!(
        render_highlighted_html(
            "<span class=\"hljs-string\">a<span class=\"hljs-subst\">${x}</span>b</span>",
            &theme
        ),
        "[string:a][string:${x}][string:b]"
    );
}
#[test]
fn upstream_renderer_keeps_parent_across_unscoped_span() {
    let theme = HighlightTheme::from([(
        "string".to_owned(),
        Box::new(|text: &str| format!("[string:{text}]")) as HighlightFormatter,
    )]);
    assert_eq!(
        render_highlighted_html(
            "<span class=\"hljs-string\">a<span class=\"language-xml\">b</span>c</span>",
            &theme
        ),
        "[string:a][string:b][string:c]"
    );
}
// senpi syntax-highlight.test.ts "loads the twenty most common languages at startup and defers the rest".
#[test]
fn upstream_eager_languages_answer_and_uncommon_ones_do_not() {
    for language in EAGER_HIGHLIGHT_LANGUAGES {
        assert!(supports_language(language), "{language}");
    }
    for alias in ["TS", "py", "sh", "rs", "c#", "jsonc", "JSON5"] {
        assert!(supports_language(alias), "{alias}");
    }
    assert!(!supports_language("ada"));
}
#[test]
fn highlights_code_through_highlight_js() {
    assert!(maho_interactive::theme::supports_language("typescript"));
    let theme = HighlightTheme::from([
        (
            "keyword".to_owned(),
            Box::new(|text: &str| format!("[keyword:{text}]")) as HighlightFormatter,
        ),
        (
            "number".to_owned(),
            Box::new(|text: &str| format!("[number:{text}]")) as HighlightFormatter,
        ),
    ]);
    let rendered = highlight("const value = 1", "typescript", &theme).expect("typescript");
    assert!(rendered.contains("[keyword:const]"), "{rendered:?}");
    assert!(rendered.contains("[number:1]"), "{rendered:?}");
}
