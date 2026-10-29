//! §8.2 schema/defaults, §8.3 validation order, §8.4 dates, §8.5 UTF-16 length, and the
//! tool surface of §2 (deferred, searchable, parallel).

use maho_ext_api::{ToolExecutionMode, ToolExposure};
use maho_x_search::params::{Mode, schema, validate};
use maho_x_search::{ErrorCode, extension::definition};
use serde_json::{Value, json};

fn code(input: &Value) -> ErrorCode {
    validate(input).expect_err("validation must fail").code
}

fn handles(count: usize) -> Value {
    json!(
        (0..count)
            .map(|index| format!("user{index}"))
            .collect::<Vec<_>>()
    )
}

#[test]
fn defaults_are_applied() {
    let params = validate(&json!({"query": "rust"})).expect("valid");
    assert_eq!(params.mode, Mode::Latest);
    assert_eq!(params.max_results, 10);
    assert!(!params.enable_image_understanding);
    assert!(!params.enable_video_understanding);
    assert_eq!(params.from_date, None);
    assert_eq!(params.allowed_x_handles, None);
}

#[test]
fn schema_rejects_additional_properties() {
    let schema = schema();
    assert_eq!(schema["additionalProperties"], json!(false));
    assert_eq!(schema["required"], json!(["query"]));
    assert_eq!(
        schema["properties"]["mode"]["enum"],
        json!(["latest", "top"])
    );
    assert_eq!(schema["properties"]["max_results"]["maximum"], json!(30));
    assert_eq!(
        code(&json!({"query": "rust", "extra": 1})),
        ErrorCode::InvalidParams
    );
}

#[test]
fn tool_surface_is_deferred_searchable_and_parallel() {
    let tool = definition();
    assert_eq!(tool.name, "x_search");
    assert_eq!(tool.exposure, Some(ToolExposure::Search));
    assert_eq!(tool.search_group.as_deref(), Some("x-search"));
    assert_eq!(tool.execution_mode, Some(ToolExecutionMode::Parallel));
    assert!(tool.description.contains("xAI"));
    assert!(tool.description.contains("credential"));
    use maho_ext_tool_search::engine::bm25::{Bm25SearchOptions, build_bm25_index};
    use maho_ext_tool_search::engine::document::{ToolSearchDocument, ToolSearchSource};
    let document = ToolSearchDocument {
        name: tool.name.clone(),
        label: tool.label.clone(),
        aliases: Vec::new(),
        description: Some(tool.description.clone()),
        search_text: tool.search_text.clone(),
        keywords: tool.search_keywords.clone().unwrap_or_default(),
        source: ToolSearchSource::Extension,
        group: tool.search_group.clone().unwrap_or_default(),
        owner_label: "x-search".into(),
        registration_id: "x-search\0x_search".into(),
    };
    let decoy = ToolSearchDocument {
        name: "read".into(),
        label: "read".into(),
        aliases: Vec::new(),
        description: Some("Read a file from disk".into()),
        search_text: None,
        keywords: Vec::new(),
        source: ToolSearchSource::Extension,
        group: "core".into(),
        owner_label: "core".into(),
        registration_id: "core\0read".into(),
    };
    let index = build_bm25_index(&[document, decoy]);
    for query in ["X posts", "tweets", "Twitter"] {
        let hits = index.search(query, 5, &Bm25SearchOptions::default());
        assert_eq!(
            hits.first().map(|hit| hit.name.as_str()),
            Some("x_search"),
            "tool search for {query:?}"
        );
    }
}

#[test]
fn validation_rows_yield_their_codes_in_order() {
    // 1. Not an object.
    assert_eq!(code(&json!("rust")), ErrorCode::InvalidParams);
    assert_eq!(code(&json!(["rust"])), ErrorCode::InvalidParams);
    // 2. Blank query.
    assert_eq!(code(&json!({"query": "  \t"})), ErrorCode::InvalidParams);
    // 3. Both handle lists.
    assert_eq!(
        code(&json!({"query": "q", "allowed_x_handles": ["a"], "excluded_x_handles": ["b"]})),
        ErrorCode::InvalidFilters
    );
    // 4. Too many handles; the message names the key.
    let failure =
        validate(&json!({"query": "q", "excluded_x_handles": handles(21)})).expect_err("too many");
    assert_eq!(failure.code, ErrorCode::TooManyHandles);
    assert!(failure.message.contains("excluded_x_handles"));
    assert!(validate(&json!({"query": "q", "allowed_x_handles": handles(20)})).is_ok());
    // 5. Bad date; the message names the key.
    let failure = validate(&json!({"query": "q", "to_date": 20240101})).expect_err("date");
    assert_eq!(failure.code, ErrorCode::InvalidDate);
    assert!(failure.message.contains("to_date"));
    // 6. Range.
    assert_eq!(
        code(&json!({"query": "q", "from_date": "2024-02-02", "to_date": "2024-02-01"})),
        ErrorCode::InvalidDateRange
    );
    // 7. Remaining schema violations.
    for input in [
        json!({}),
        json!({"query": 5}),
        json!({"query": "q", "mode": "oldest"}),
        json!({"query": "q", "max_results": 0}),
        json!({"query": "q", "max_results": 31}),
        json!({"query": "q", "max_results": 2.5}),
        json!({"query": "q", "max_results": "5"}),
        json!({"query": "q", "allowed_x_handles": []}),
        json!({"query": "q", "allowed_x_handles": ["a", "a"]}),
        json!({"query": "q", "allowed_x_handles": ["@a"]}),
        json!({"query": "q", "allowed_x_handles": ["sixteen_chars_xx"]}),
        json!({"query": "q", "allowed_x_handles": "a"}),
        json!({"query": "q", "enable_image_understanding": "yes"}),
        json!({"query": "q", "unknown": true}),
    ] {
        assert_eq!(code(&input), ErrorCode::InvalidParams, "{input}");
    }
}

#[test]
fn earlier_rows_win_over_later_ones() {
    // Blank query beats both handle lists.
    assert_eq!(
        code(&json!({"query": " ", "allowed_x_handles": ["a"], "excluded_x_handles": ["b"]})),
        ErrorCode::InvalidParams
    );
    // Both lists beat 21 handles.
    assert_eq!(
        code(&json!({"query": "q", "allowed_x_handles": handles(21), "excluded_x_handles": ["b"]})),
        ErrorCode::InvalidFilters
    );
    // Too many handles beats a bad date; a bad date beats an unknown key.
    assert_eq!(
        code(&json!({"query": "q", "allowed_x_handles": handles(21), "from_date": "bad"})),
        ErrorCode::TooManyHandles
    );
    assert_eq!(
        code(&json!({"query": "q", "from_date": "bad", "unknown": 1})),
        ErrorCode::InvalidDate
    );
}

#[test]
fn dates_must_be_real_calendar_days() {
    let with = |date: &str| validate(&json!({"query": "q", "from_date": date}));
    assert!(with("2024-02-29").is_ok());
    assert!(with("2000-02-29").is_ok());
    for bad in [
        "2023-02-29",
        "1900-02-29",
        "2024-13-01",
        "0099-01-01",
        "2024-1-01",
        "2024-04-31",
        "2024-00-10",
        "２０２４-01-01",
        "2024-01-01T00",
    ] {
        assert_eq!(
            with(bad).expect_err(bad).code,
            ErrorCode::InvalidDate,
            "{bad}"
        );
    }
    assert_eq!(
        code(&json!({"query": "q", "from_date": "2024-03-01", "to_date": "2024-02-29"})),
        ErrorCode::InvalidDateRange
    );
    assert!(
        validate(&json!({"query": "q", "from_date": "2024-03-01", "to_date": "2024-03-01"}))
            .is_ok()
    );
}

#[test]
fn query_length_counts_utf16_units() {
    // U+1F600 is two UTF-16 units.
    let emoji = "\u{1F600}";
    assert!(validate(&json!({"query": emoji.repeat(1000)})).is_ok());
    assert_eq!(
        code(&json!({"query": format!("{}a", emoji.repeat(1000))})),
        ErrorCode::InvalidParams
    );
    assert!(validate(&json!({"query": "a".repeat(2000)})).is_ok());
    assert_eq!(
        code(&json!({"query": "a".repeat(2001)})),
        ErrorCode::InvalidParams
    );
}
