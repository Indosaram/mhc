use maho_interactive::{components::footer::{FooterComponent, FooterSnapshot, format_tokens}, theme::{ColorMode, Theme, ThemeColor, ThemeBg}};
use serde_json::Value;
#[test]
fn original_footer_corpus_is_byte_equal_at_required_widths() {
    let snapshot: FooterSnapshot = serde_json::from_str(include_str!("golden/footer-input.json")).unwrap();
    let footer = FooterComponent::new(snapshot);
    let theme = Theme::builtin("dark", ColorMode::Truecolor).unwrap();
    for width in [40, 60, 80, 120, 200] {
        let expected = std::fs::read_to_string(format!("{}/tests/golden/footer.{width}.ansi", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(footer.render(width, &theme).unwrap().join("\n"), expected);
    }
}
#[test]
fn all_theme_tokens_match_pinned_palette_ansi() {
    let palettes: Vec<Value> = serde_json::from_str(include_str!("golden/palettes.json")).unwrap();
    for palette in palettes {
        let theme = Theme::builtin(palette["name"].as_str().unwrap(), ColorMode::Truecolor).unwrap();
        for color in ThemeColor::ALL { assert_eq!(theme.fg(*color, "sample"), palette["fg"][color.key()].as_str().unwrap()); }
        for color in ThemeBg::ALL { assert_eq!(theme.bg(*color, "sample"), palette["bg"][color.key()].as_str().unwrap()); }
    }
}
#[test]
fn abbreviated_tokens_match_pinned_values() {
    let corpus: Value = serde_json::from_str(include_str!("golden/format-tokens.json")).unwrap();
    for pair in corpus["tokens"].as_array().unwrap() { assert_eq!(format_tokens(pair[0].as_f64().unwrap()), pair[1].as_str().unwrap()); }
}
