//! Golden compare against the senpi-generated Text fixtures (tools/golden/cases/tui-text-basic.json).

use maho_test_support::golden::{
    GoldenError, assert_golden_lines, compare, compare_lines, fixture_path,
};

/// Stand-in for the Rust Text port: returns the lines senpi's Text(s, 1, 1).render(w) produces.
fn text_stub(text: &str, width: usize) -> Vec<String> {
    let content_width = width - 2;
    let mut lines = vec![" ".repeat(width)];
    let mut current = String::new();
    for word in text.split(' ') {
        if !current.is_empty() && current.len() + 1 + word.len() > content_width {
            lines.push(pad(&current, width));
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    lines.push(pad(&current, width));
    lines.push(" ".repeat(width));
    lines
}

fn pad(content: &str, width: usize) -> String {
    format!(" {content}{}", " ".repeat(width - 1 - content.len()))
}

const TEXT: &str = "Hello, world. This line is long enough to wrap at forty columns.";

#[test]
fn stub_matches_senpi_text_fixture_at_every_width() {
    for width in [40, 80, 120] {
        let path = fixture_path("maho-tui", &format!("tui-text-basic.{width}.ansi"));
        assert_golden_lines(&path, &text_stub(TEXT, width));
    }
}

#[test]
fn one_flipped_byte_fails_with_a_unified_diff() {
    let path = fixture_path("maho-tui", "tui-text-basic.40.ansi");
    let mut lines = text_stub(TEXT, 40);
    lines[1] = lines[1].replacen("Hello", "Jello", 1);
    let Err(GoldenError::Mismatch { diff, .. }) = compare_lines(&path, &lines) else {
        panic!("a flipped byte must be a mismatch");
    };
    eprintln!("{diff}");
    assert!(diff.contains("--- expected (golden)"), "{diff}");
    assert!(diff.contains("- Hello, world."), "{diff}");
    assert!(diff.contains("+ Jello, world."), "{diff}");
}

#[test]
fn missing_fixture_is_an_error_and_is_not_created() {
    let path = fixture_path("maho-test-support", "does-not-exist.ansi");
    assert!(matches!(compare(&path, "x"), Err(GoldenError::Read { .. })));
    assert!(!path.exists());
}

#[test]
fn escapes_make_ansi_differences_visible() {
    let diff = maho_test_support::golden::unified_diff("\x1b[1mA\x1b[0m", "\x1b[2mA\x1b[0m");
    assert!(diff.contains("-\\e[1mA\\e[0m"), "{diff}");
    assert!(diff.contains("+\\e[2mA\\e[0m"), "{diff}");
}
