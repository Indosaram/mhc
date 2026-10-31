//! Port of senpi `test/suite/webfetch-golden.test.ts` and the converter case of
//! `test/suite/webfetch-document.test.ts` (pin 903b8336); the parser cases of that file live in
//! `src/webfetch/parse_web_document.rs`. Fixtures under `tests/fixtures/webfetch/` are copied byte-for-byte from
//! `packages/coding-agent/test/fixtures/webfetch/` at that pin.

use maho_ext_webfetch::webfetch::{
    content::{html_to_markdown, html_to_text},
    tool::{DEFAULT_OUTPUT_MAX_BYTES, cap_webfetch_output},
};

const FIXTURES: [&str; 15] = [
    "01-reader",
    "02-explicit",
    "03-tistory",
    "04-title",
    "05-lines",
    "06-entities",
    "07-redirect",
    "08-readable-urls",
    "09-explicit-urls",
    "10-base-redirect",
    "11-malformed",
    "12-multibyte",
    "13-omitted-body",
    "14-html-in-comment-script",
    "15-fragment",
];
/// Recorded final URL: fixture 10 is served after /fixtures/base/start redirects here.
const FINAL_URL: &str = "https://example.test/fixtures/base/final";

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/webfetch")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// Same normalization as upstream: CRLF to LF, trailing spaces and tabs per line, one final newline.
fn normalize(value: &str) -> String {
    let joined = value
        .replace("\r\n", "\n")
        .split('\n')
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join("\n");
    joined.strip_suffix('\n').unwrap_or(&joined).to_owned()
}

fn convert(format: &str, html: &str) -> String {
    if format == "md" {
        html_to_markdown(html, FINAL_URL)
    } else {
        html_to_text(html, FINAL_URL)
    }
}

/// Upstream's allowed delta for fixtures 9 and 10: relative markdown destinations resolve against
/// the final URL (fixture 9, `#section` stays relative) or the first base href (fixture 10).
fn expected_with_resolved_destinations(fixture_name: &str, expected: String) -> String {
    let base = if fixture_name == "10-base-redirect" {
        url::Url::parse(FINAL_URL)
            .and_then(|url| url.join("../assets/"))
            .expect("base")
            .to_string()
    } else {
        FINAL_URL.to_owned()
    };
    let mut output = expected;
    for value in ["../guide", "images/example.png", "#section"] {
        if value.starts_with('#') && base == FINAL_URL {
            continue;
        }
        let resolved = url::Url::parse(&base)
            .and_then(|url| url.join(value))
            .expect("resolved");
        output = output.replace(&format!("]({value})"), &format!("]({resolved})"));
    }
    output
}

#[test]
fn preserves_output_when_converting_every_golden_fixture() {
    for name in FIXTURES {
        let html = fixture(&format!("{name}.html"));
        for format in ["md", "txt"] {
            let mut expected = fixture(&format!("{name}.{format}.golden"));
            if format == "md" && matches!(name, "09-explicit-urls" | "10-base-redirect") {
                expected = expected_with_resolved_destinations(name, expected);
            }
            assert_eq!(
                normalize(&convert(format, &html)),
                normalize(&expected),
                "{name}.{format}"
            );
        }
    }
}

#[test]
fn preserves_output_when_document_tags_comment_or_script_are_omitted() {
    let omitted_document_tags = regex::Regex::new(r"</?(?:html|head)>").expect("regex");
    let comment = regex::Regex::new(r"<!--.*?-->").expect("regex");
    let script = regex::Regex::new(r"<script>.*?</script>").expect("regex");
    let cases = [
        ("13-omitted-body", &omitted_document_tags, true),
        ("14-html-in-comment-script", &comment, false),
        ("14-html-in-comment-script", &script, false),
    ];
    for (name, omitted, global) in cases {
        let source = fixture(&format!("{name}.html"));
        let html = if global {
            omitted.replace_all(&source, "").into_owned()
        } else {
            omitted.replace(&source, "").into_owned()
        };
        for format in ["md", "txt"] {
            let expected = fixture(&format!("{name}.{format}.golden"));
            assert_eq!(
                normalize(&convert(format, &html)),
                normalize(&expected),
                "{name}.{format} without {}",
                omitted.as_str()
            );
        }
    }
}

#[test]
fn caps_complete_utf8_output_when_the_multibyte_article_exceeds_50_kib() {
    let markdown = html_to_markdown(&fixture("12-multibyte.html"), FINAL_URL);
    let capped = cap_webfetch_output(&markdown);
    assert!(capped.truncated);
    assert!(capped.total_bytes > DEFAULT_OUTPUT_MAX_BYTES);
    assert!(capped.output_bytes <= DEFAULT_OUTPUT_MAX_BYTES);
    assert!(!capped.text.contains('\u{FFFD}'));
}

const DOCUMENT_URL: &str = "https://example.test/posts/final";

#[test]
fn keeps_url_state_isolated_when_converting_consecutive_pages() {
    let html = r#"<div class="article_view"><p>Article content long enough for explicit selection with a <a href="child">link</a>.</p></div>"#;
    html_to_markdown(html, "https://previous.test/first/");
    let markdown = html_to_markdown(html, DOCUMENT_URL);
    assert!(
        markdown.contains("](https://example.test/posts/child)"),
        "{markdown}"
    );
    assert!(!markdown.contains("previous.test"));
}
