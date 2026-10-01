//! Byte-exact Markdown parity with senpi.
//!
//! Each `tools/golden/cases/tui-markdown.json` document is rendered at every width in the case
//! with the `chalk` theme and compared byte-for-byte with the `.ansi` fixture senpi produced.

use maho_tui::components::markdown::{Markdown, MarkdownOptions, MarkdownTheme, ThemeFn};
use serde_json::Value;

fn wrap(open: &'static str, close: &'static str) -> ThemeFn {
    std::sync::Arc::new(move |text: &str| format!("\u{1b}[{open}m{text}\u{1b}[{close}m"))
}

fn chalk_theme() -> MarkdownTheme {
    let bold = wrap("1", "22");
    let dim = wrap("2", "22");
    let italic = wrap("3", "23");
    let underline = wrap("4", "24");
    let strikethrough = wrap("9", "29");
    let cyan = wrap("36", "39");
    let blue = wrap("34", "39");
    let yellow = wrap("33", "39");
    let green = wrap("32", "39");
    MarkdownTheme {
        id: 9_001,
        heading: {
            let (bold, cyan) = (bold.clone(), cyan.clone());
            std::sync::Arc::new(move |text: &str| bold(&cyan(text)))
        },
        link: blue,
        link_url: dim.clone(),
        code: yellow,
        code_block: green,
        code_block_border: dim.clone(),
        quote: italic.clone(),
        quote_border: dim.clone(),
        hr: dim,
        list_bullet: cyan,
        bold,
        italic,
        strikethrough,
        underline,
        highlight_code: None,
        code_block_indent: None,
    }
}

fn case() -> Value {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/golden/cases/tui-markdown.json");
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn golden(name: &str) -> String {
    let path = format!("{}/tests/golden/{name}.ansi", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

#[test]
fn renders_every_case_document_byte_for_byte_at_every_width() {
    let case = case();
    let widths: Vec<usize> = case["widths"]
        .as_array()
        .expect("widths")
        .iter()
        .map(|width| usize::try_from(width.as_u64().expect("width")).expect("fits usize"))
        .collect();
    let docs = case["docs"].as_array().expect("docs");
    assert!(!docs.is_empty(), "case has no documents");

    let mut checked = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for doc in docs {
        let id = doc["id"].as_str().expect("doc id");
        let text = doc["text"].as_str().expect("doc text");
        let padding_x = usize::try_from(doc["paddingX"].as_u64().unwrap_or(0)).expect("paddingX");
        let padding_y = usize::try_from(doc["paddingY"].as_u64().unwrap_or(0)).expect("paddingY");
        for &width in &widths {
            let name = format!("tui-markdown.{id}.{width}");
            let mut markdown = Markdown::new(
                text,
                padding_x,
                padding_y,
                chalk_theme(),
                None,
                MarkdownOptions::default(),
            );
            let actual = markdown.render(width).join("\n");
            let expected = golden(&name);
            checked += 1;
            if actual != expected {
                let index = actual
                    .chars()
                    .zip(expected.chars())
                    .position(|(a, b)| a != b)
                    .unwrap_or_else(|| actual.chars().count().min(expected.chars().count()));
                let window = |text: &str| {
                    text.chars()
                        .skip(index.saturating_sub(30))
                        .take(80)
                        .collect::<String>()
                        .escape_debug()
                        .to_string()
                };
                failures.push(format!(
                    "{name}: first difference at character {index}\n    senpi: {}\n    maho:  {}",
                    window(&expected),
                    window(&actual)
                ));
            }
        }
    }

    assert_eq!(checked, docs.len() * widths.len(), "every document at every width");
    assert!(failures.is_empty(), "{} of {checked} mismatched:\n{}", failures.len(), failures.join("\n"));
}
