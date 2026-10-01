use maho_server::app_server::search_params::parse_search_params;
use serde_json::json;
#[test]
fn search_defaults_normalize_and_clamp_unsigned_limits() {
    let parsed = parse_search_params(&json!({"searchTerm":"  HeLLo 한글  "})).unwrap();
    assert_eq!(parsed.search_term, "hello 한글");
    assert_eq!(parsed.limit, 25);
    assert_eq!(parsed.source_kinds, ["cli", "vscode"]);
    assert_eq!(parsed.sort_key, "created_at");
    assert_eq!(parsed.sort_direction, "desc");
    assert!(!parsed.archived);
    assert_eq!(parsed.cursor, None);
    for (input, expected) in [(0.0, 1), (1.0, 1), (100.0, 100), (4294967295.0, 100)] {
        assert_eq!(
            parse_search_params(&json!({"searchTerm":"x","limit":input}))
                .unwrap()
                .limit,
            expected
        );
    }
}
#[test]
fn sources_are_validated_sorted_and_deduplicated() {
    let parsed=parse_search_params(&json!({"searchTerm":"x","sourceKinds":["vscode","cli","cli","subAgent"],"cursor":"","archived":true,"sortKey":"recency_at","sortDirection":"asc"})).unwrap();
    assert_eq!(parsed.source_kinds, ["cli", "subAgent", "vscode"]);
    assert_eq!(parsed.cursor, Some("".into()));
    assert!(parsed.archived);
    assert_eq!(
        parse_search_params(&json!({"searchTerm":"x","sourceKinds":[]}))
            .unwrap()
            .source_kinds,
        ["cli", "vscode"]
    );
}
#[test]
fn invalid_values_return_invalid_request_not_params() {
    for value in [
        json!(null),
        json!([]),
        json!({"searchTerm":" "}),
        json!({"searchTerm":"x","limit":-1}),
        json!({"searchTerm":"x","limit":0.5}),
        json!({"searchTerm":"x","limit":4294967296.0}),
        json!({"searchTerm":"x","sourceKinds":["bad"]}),
        json!({"searchTerm":"x","cursor":1}),
        json!({"searchTerm":"x","archived":"false"}),
        json!({"searchTerm":"x","sortKey":"bad"}),
        json!({"searchTerm":"x","sortDirection":"bad"}),
    ] {
        assert_eq!(parse_search_params(&value).unwrap_err().code, -32600);
    }
}
