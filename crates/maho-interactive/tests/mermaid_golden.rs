use maho_interactive::grok_mermaid::{render, Cls};
use serde_json::Value;

fn fixture() -> Value {
    serde_json::from_str(include_str!("golden/mermaid-hg.json")).expect("upstream fixture")
}

fn class_name(cls: Cls) -> &'static str {
    match cls {
        Cls::Border => "border",
        Cls::Text => "text",
        Cls::Edge => "edge",
        Cls::EdgeLabel => "edgeLabel",
        Cls::Title => "title",
        Cls::None => "none",
    }
}

#[test]
fn grok_mermaid_matches_pinned_upstream_goldens() {
    let fixture = fixture();
    assert_eq!(fixture["provenance"]["senpiCommit"], "fe8c564bf33a2cbbdbbba99c9bd8b45b21e37407");
    assert_eq!(fixture["provenance"]["package"], "grok-mermaid");
    assert_eq!(fixture["provenance"]["version"], "0.2.3");

    for case in fixture["cases"].as_array().expect("cases") {
        let name = case["name"].as_str().expect("case name");
        let source = case["source"].as_str().expect("diagram source");
        let expected = &case["result"];
        let actual = render(source).unwrap_or_else(|| panic!("{name}: expected rendered art"));

        let expected_plain: Vec<&str> = expected["plain"]
            .as_array()
            .expect("plain rows")
            .iter()
            .map(|line| line.as_str().expect("plain row"))
            .collect();
        assert_eq!(actual.plain, expected_plain, "{name}: plain output");

        let expected_styled = expected["styled"].as_array().expect("styled rows");
        assert_eq!(actual.styled.len(), expected_styled.len(), "{name}: styled row count");
        for (row_index, (actual_row, expected_row)) in actual.styled.iter().zip(expected_styled).enumerate() {
            assert_eq!(actual_row.len(), expected_row.as_array().expect("styled row").len(), "{name}: spans in row {row_index}");
            for (span_index, (span, expected_span)) in actual_row.iter().zip(expected_row.as_array().expect("styled row")).enumerate() {
                assert_eq!(span.text, expected_span["text"].as_str().expect("span text"), "{name}: text at {row_index}:{span_index}");
                assert_eq!(class_name(span.cls), expected_span["cls"].as_str().expect("span class"), "{name}: class at {row_index}:{span_index}");
            }
        }

        assert_eq!(actual.width, expected["width"].as_u64().expect("diagram width") as usize, "{name}: width");
        let expected_warnings: Vec<&str> = expected["warnings"]
            .as_array()
            .expect("warnings")
            .iter()
            .map(|warning| warning.as_str().expect("warning"))
            .collect();
        assert_eq!(actual.warnings, expected_warnings, "{name}: warnings");
    }
}
