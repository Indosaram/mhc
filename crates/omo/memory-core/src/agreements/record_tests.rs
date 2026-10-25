use pretty_assertions::assert_eq;
use serde_json::json;

use super::{
    agreement_text_problem, clean_agreement_text, parse_agreement_record,
    parse_agreement_records_jsonl,
};
use crate::agreements::types::{AgreementExtractionRecord, AgreementScope};

#[test]
fn parses_new_and_reaffirm_records() {
    let new = parse_agreement_record(
        &json!({"action":"new","applies":"project","text":"  Run   tests ","date":"2026-02-28"}),
        0,
    );
    assert_eq!(
        new.expect("new"),
        AgreementExtractionRecord::New {
            applies: AgreementScope::Project,
            text: "Run tests".to_string(),
            date: "2026-02-28".to_string(),
            source: None,
        }
    );
    let reaffirm = parse_agreement_record(
        &json!({"action":"reaffirm","id":"0123456789","date":"2026-02-28"}),
        0,
    );
    assert_eq!(
        reaffirm.expect("reaffirm"),
        AgreementExtractionRecord::Reaffirm {
            id: "0123456789".to_string(),
            date: "2026-02-28".to_string(),
            text: None,
            source: None,
        }
    );
}

#[test]
fn rejects_invalid_records_with_one_based_prefix() {
    for (value, message) in [
        (json!([]), "record must be an object"),
        (json!({"action":"new"}), "date must be YYYY-MM-DD"),
        (
            json!({"action":"new","applies":"global","text":"x","date":"2026-02-30"}),
            "date must be YYYY-MM-DD",
        ),
        (
            json!({"action":"x","date":"2026-01-01"}),
            "action must be new or reaffirm",
        ),
        (
            json!({"action":"new","applies":"global","text":"  ","date":"2026-01-01"}),
            "text must be non-empty",
        ),
        (
            json!({"action":"new","applies":"team","text":"x","date":"2026-01-01"}),
            "applies must be global or project",
        ),
        (
            json!({"action":"new","applies":"global","text":"a <!-- b","date":"2026-01-01"}),
            "text must not contain HTML comment markers",
        ),
        (
            json!({"action":"reaffirm","id":"nope","date":"2026-01-01"}),
            "id must name an existing agreement",
        ),
        (
            json!({"action":"reaffirm","id":"0123456789","date":"2026-01-01","extra":1}),
            "unexpected field: extra",
        ),
    ] {
        let error = parse_agreement_record(&value, 2).expect_err("invalid");
        assert_eq!(error.message, format!("agreement record 3: {message}"));
    }
}

#[test]
fn jsonl_skips_blank_lines_and_reports_bad_json() {
    let ok = parse_agreement_records_jsonl(
        "\n{\"action\":\"reaffirm\",\"id\":\"0123456789\",\"date\":\"2026-01-01\"}\n",
    )
    .expect("parse");
    assert_eq!(ok.len(), 1);
    let error = parse_agreement_records_jsonl("{").expect_err("bad json");
    assert_eq!(error.message, "agreement record 1: line is not valid JSON");
}

#[test]
fn text_problems() {
    assert_eq!(clean_agreement_text("  a \n b  "), "a b");
    assert_eq!(
        agreement_text_problem(""),
        Some("text must be non-empty".to_string())
    );
    assert_eq!(
        agreement_text_problem(&"x".repeat(241)),
        Some("text must be at most 240 characters".to_string())
    );
    assert_eq!(agreement_text_problem(&"x".repeat(240)), None);
}
