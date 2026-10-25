use super::*;
use crate::agreements::{parse_agreement_record, parse_agreements_ledger};
use serde_json::json;

#[test]
fn receipts_are_strict_unique_and_sorted() {
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    let lines = serialize_agreement_events(&[b.clone(), a.clone(), a.clone()]);
    assert_eq!(lines.len(), 2);
    assert_eq!(parse_agreement_event_line(&lines[0]), Some(a.clone()));
    for bad in [
        format!("<!-- agreement-event: {} -->", "A".repeat(64)),
        format!("<!-- agreement-event: {a} --> trailing"),
        "<!-- agreement-event: abc -->".to_string(),
    ] {
        assert_eq!(parse_agreement_event_line(&bad), None);
        assert_eq!(parse_agreements_ledger(&bad).diagnostics.len(), 1);
    }
    let parsed = parse_agreements_ledger(&format!("{}\n{}\n{}", lines[0], lines[1], lines[0]));
    assert_eq!(parsed.receipts, vec![a, b]);
    assert!(parsed.unparsed.is_empty());
    assert!(parsed.diagnostics.is_empty());
}

#[test]
fn keys_use_source_identity_instead_of_date() {
    let record = |date: &str, session: &str, message: &str| {
        parse_agreement_record(
            &json!({
                "action":"new", "applies":"global", "text":"Run tests", "date":date,
                "source":{"sessionId":session,"messageId":message}
            }),
            0,
        )
        .unwrap()
    };
    let first = agreement_event_key("0123456789", &record("2026-10-01", "s", "m"));
    assert_eq!(
        first,
        agreement_event_key("0123456789", &record("2026-10-02", "s", "m"))
    );
    assert_ne!(
        first,
        agreement_event_key("0123456789", &record("2026-10-01", "s2", "m"))
    );
    assert_ne!(
        first,
        legacy_agreement_event_key("0123456789", "2026-10-01")
    );
}
