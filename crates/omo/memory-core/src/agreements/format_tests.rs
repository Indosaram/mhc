use pretty_assertions::assert_eq;

use super::{agreement_id, parse_agreements_ledger, serialize_agreements_ledger, sort_agreements};
use crate::agreements::types::{AgreementAuthor, AgreementItem, AgreementScope, AgreementStatus};

fn item(id: &str, by: AgreementAuthor, since: Option<&str>) -> AgreementItem {
    AgreementItem {
        id: id.to_string(),
        scope: AgreementScope::Global,
        by,
        text: format!("text {id}"),
        n: 0,
        since: since.map(str::to_string),
        last: None,
        status: AgreementStatus::Active,
    }
}

#[test]
fn id_is_ten_hex_and_ignores_case_and_trailing_punctuation() {
    assert_eq!(
        agreement_id("Run the tests."),
        agreement_id("run  the TESTS")
    );
    assert_eq!(agreement_id("x").len(), 10);
}

#[test]
fn round_trips_a_full_line() {
    let line = "- [project] Ask before deleting <!-- id: 0123456789; by: human; n: 2; since: 2026-01-02; last: 2026-02-03; status: needs-human -->";
    let parsed = parse_agreements_ledger(line);
    assert_eq!(parsed.diagnostics, Vec::<String>::new());
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(parsed.items[0].n, 2);
    assert_eq!(parsed.items[0].status, AgreementStatus::NeedsHuman);
    assert_eq!(serialize_agreements_ledger(&parsed.items, &[]), line);
}

#[test]
fn bare_line_gets_defaults_and_derived_id() {
    let parsed = parse_agreements_ledger("- [global] Keep commits small");
    assert_eq!(parsed.items[0].by, AgreementAuthor::Agent);
    assert_eq!(parsed.items[0].id, agreement_id("Keep commits small"));
}

#[test]
fn reports_unparseable_and_duplicate_lines() {
    let body = "garbage\n- [global] Same <!-- id: 0123456789 -->\n- [global] Same again <!-- id: 0123456789 -->\n- [project] Bad <!-- by: robot -->\n";
    let parsed = parse_agreements_ledger(body);
    assert_eq!(parsed.items.len(), 1);
    assert_eq!(
        parsed.diagnostics,
        vec![
            "line 1: expected \"- [global|project] <agreement>\"".to_string(),
            "line 3: duplicate agreement id 0123456789".to_string(),
            "line 4: by must be human or agent".to_string(),
        ]
    );
    assert_eq!(parsed.unparsed.len(), 3);
}

#[test]
fn rejects_bad_meta_values() {
    for (meta, message) in [
        ("id: ABC", "id must be 10 lowercase hex characters"),
        ("n: -1", "n must be a non-negative integer"),
        ("n: 1.5", "n must be a non-negative integer"),
        ("status: done", "status must be active or needs-human"),
        ("since: 2026-1-1", "since must be YYYY-MM-DD"),
        ("last: soon", "last must be YYYY-MM-DD"),
        (
            "nonsense",
            "metadata segment \"nonsense\" is not key: value",
        ),
    ] {
        let parsed = parse_agreements_ledger(&format!("- [global] Text <!-- {meta} -->"));
        assert_eq!(
            parsed.diagnostics,
            vec![format!("line 1: {message}")],
            "{meta}"
        );
    }
}

#[test]
fn sorts_human_first_then_since_then_id() {
    let sorted = sort_agreements(&[
        item("bbbbbbbbbb", AgreementAuthor::Agent, Some("2026-01-01")),
        item("aaaaaaaaaa", AgreementAuthor::Agent, Some("2026-01-01")),
        item("cccccccccc", AgreementAuthor::Human, Some("2026-03-01")),
        item("dddddddddd", AgreementAuthor::Agent, None),
    ]);
    let ids: Vec<&str> = sorted.iter().map(|entry| entry.id.as_str()).collect();
    assert_eq!(
        ids,
        ["cccccccccc", "dddddddddd", "aaaaaaaaaa", "bbbbbbbbbb"]
    );
}

#[test]
fn serialization_appends_unparsed_lines() {
    let out = serialize_agreements_ledger(&[], &["keep me".to_string()]);
    assert_eq!(out, "keep me");
}
