//! Port of omo-senpi `components/x-search/params.test.ts` at latest `455dee62`.

use maho_omo_x_search::format::XSearchErrorCode;
use maho_omo_x_search::params::{validate_x_search_params, XSearchParams, XSearchValidation};
use serde_json::json;

#[test]
fn test_accepts_valid_minimal_payload_and_applies_defaults() {
    let result = validate_x_search_params(&json!({ "query": "open source AI" }));
    assert_eq!(
        result,
        XSearchValidation::Ok(XSearchParams {
            query: "open source AI".to_string(),
            from_date: None,
            to_date: None,
            allowed_x_handles: None,
            excluded_x_handles: None,
            mode: "latest".to_string(),
            max_results: 10,
            enable_image_understanding: false,
            enable_video_understanding: false,
        })
    );
}

#[test]
fn test_rejects_blank_query_as_invalid_params() {
    let result = validate_x_search_params(&json!({ "query": "   " }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidParams);
    assert_eq!(message, "query must not be blank");
}

#[test]
fn test_rejects_both_handle_lists_as_invalid_filters() {
    let result = validate_x_search_params(&json!({
        "query": "x",
        "allowed_x_handles": ["alice"],
        "excluded_x_handles": ["bob"]
    }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidFilters);
    assert_eq!(message, "allowed and excluded handles are mutually exclusive");
}

#[test]
fn test_rejects_more_than_20_handles_as_too_many_handles() {
    let handles: Vec<String> = (0..21).map(|i| format!("user{i}")).collect();
    let result = validate_x_search_params(&json!({
        "query": "x",
        "allowed_x_handles": handles
    }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::TooManyHandles);
    assert_eq!(message, "allowed_x_handles cannot contain more than 20 handles");
}

#[test]
fn test_rejects_impossible_calendar_dates_as_invalid_date() {
    let result = validate_x_search_params(&json!({
        "query": "x",
        "from_date": "2026-02-30"
    }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidDate);
    assert_eq!(message, "from_date must be a valid ISO calendar date");
}

#[test]
fn test_rejects_inverted_date_range_as_invalid_date_range() {
    let result = validate_x_search_params(&json!({
        "query": "x",
        "from_date": "2026-03-01",
        "to_date": "2026-02-01"
    }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidDateRange);
    assert_eq!(message, "from_date must be on or before to_date");
}

#[test]
fn test_rejects_unknown_properties_as_invalid_params() {
    let result = validate_x_search_params(&json!({
        "query": "x",
        "unexpected": true
    }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidParams);
    assert_eq!(message, "Parameters failed validation");
}

#[test]
fn test_rejects_non_object_as_invalid_params() {
    let result = validate_x_search_params(&json!("just a string"));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidParams);
    assert_eq!(message, "Parameters must be an object");
}

#[test]
fn test_date_utc_behavior_rejects_year_under_100() {
    // In JS Date.UTC, year 0000..0099 maps to 1900..1999, so getUTCFullYear() !== year
    let result = validate_x_search_params(&json!({
        "query": "x",
        "from_date": "0050-01-01"
    }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidDate);
    assert_eq!(message, "from_date must be a valid ISO calendar date");
}

#[test]
fn test_date_rejects_signs_or_non_ascii_digits() {
    let result = validate_x_search_params(&json!({
        "query": "x",
        "from_date": "2026-+1-01"
    }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidDate);
    assert_eq!(message, "from_date must be a valid ISO calendar date");
}

#[test]
fn test_query_max_length_measured_in_utf16_code_units() {
    // A string of 2001 ASCII chars exceeds 2000
    let long_query = "a".repeat(2001);
    let result = validate_x_search_params(&json!({ "query": long_query }));
    let XSearchValidation::Err { code, message } = result else {
        panic!("expected Err");
    };
    assert_eq!(code, XSearchErrorCode::InvalidParams);
    assert_eq!(message, "Parameters failed validation");
}

#[test]
fn test_validation_priority_blank_query_precedes_handles_and_schema() {
    let result = validate_x_search_params(&json!({
        "query": "   ",
        "allowed_x_handles": ["a"],
        "excluded_x_handles": ["b"],
        "unexpected": 123
    }));
    let XSearchValidation::Err { code, message } = result else { panic!("expected Err") };
    assert_eq!(code, XSearchErrorCode::InvalidParams);
    assert_eq!(message, "query must not be blank");
}

#[test]
fn test_validation_priority_mutual_exclusivity_precedes_handle_count() {
    let result = validate_x_search_params(&json!({
        "query": "valid",
        "allowed_x_handles": (0..25).map(|i| format!("user{i}")).collect::<Vec<_>>(),
        "excluded_x_handles": ["b"]
    }));
    let XSearchValidation::Err { code, message } = result else { panic!("expected Err") };
    assert_eq!(code, XSearchErrorCode::InvalidFilters);
    assert_eq!(message, "allowed and excluded handles are mutually exclusive");
}

#[test]
fn test_validation_priority_handle_count_precedes_invalid_dates() {
    let result = validate_x_search_params(&json!({
        "query": "valid",
        "allowed_x_handles": (0..25).map(|i| format!("user{i}")).collect::<Vec<_>>(),
        "from_date": "invalid-date"
    }));
    let XSearchValidation::Err { code, message } = result else { panic!("expected Err") };
    assert_eq!(code, XSearchErrorCode::TooManyHandles);
    assert_eq!(message, "allowed_x_handles cannot contain more than 20 handles");
}
