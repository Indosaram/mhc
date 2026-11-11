//! Parsing and validating the records an extraction model returns.
//!
//! Spec: `docs/cleanroom/specs/memory-facts-extraction.md`. Every record is checked against an exact allowed
//! key set, so a field the model invented is an error rather than something silently stored: a typo in
//! `scope` or an extra `confidence` key would otherwise be written into memory as if it were reviewed.

use chrono::NaiveDate;

use serde::{Deserialize, Serialize};

use crate::fact_display::ClaimReceipt;

/// Someone a person-scoped fact is filed under.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactsPersonReference {
    pub name: String,
    pub aliases: Vec<String>,
}

/// Receipts are kept as the parsed value rather than typed: `ClaimReceipt` is a parse gate, and re-serializing
/// it would rewrite the fields the model sent into a shape this crate invented, changing the batch hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "lowercase")]
pub enum FactsExtractionRecord {
    Project {
        text: String,
        date: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<serde_json::Value>,
    },
    Person {
        person: FactsPersonReference,
        text: String,
        date: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        receipt: Option<serde_json::Value>,
    },
}

impl FactsExtractionRecord {
    pub fn receipt(&self) -> Option<&serde_json::Value> {
        match self {
            Self::Project { receipt, .. } | Self::Person { receipt, .. } => receipt.as_ref(),
        }
    }

    pub fn text(&self) -> &str {
        match self {
            Self::Project { text, .. } | Self::Person { text, .. } => text,
        }
    }

    pub fn date(&self) -> &str {
        match self {
            Self::Project { date, .. } | Self::Person { date, .. } => date,
        }
    }

    /// The person this record belongs to, for routing. `None` is a project-scoped fact.
    pub fn person(&self) -> Option<&FactsPersonReference> {
        match self {
            Self::Project { .. } => None,
            Self::Person { person, .. } => Some(person),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactsExtractionValidationError {
    /// Already carries the 1-based line number, so a caller can report it without adding context.
    pub message: String,
}

impl std::fmt::Display for FactsExtractionValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.message)
    }
}

impl std::error::Error for FactsExtractionValidationError {}

const PROJECT_KEYS: [&str; 4] = ["scope", "text", "date", "receipt"];
const PERSON_KEYS: [&str; 5] = ["scope", "person", "text", "date", "receipt"];
const PERSON_REFERENCE_KEYS: [&str; 2] = ["name", "aliases"];

/// Parse one JSONL document: blank lines skipped, every other line one record.
///
/// Blank lines are skipped rather than rejected because a model streams trailing newlines; failing on them
/// would discard an otherwise valid batch.
pub fn parse_facts_extraction_jsonl(
    raw: &str,
) -> Result<Vec<FactsExtractionRecord>, FactsExtractionValidationError> {
    let mut records = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value =
            serde_json::from_str(trimmed).map_err(|_| invalid(index, "line is not valid JSON"))?;
        records.push(parse_facts_extraction_record(&value, index)?);
    }
    Ok(records)
}

pub fn parse_facts_extraction_record(
    value: &serde_json::Value,
    index: usize,
) -> Result<FactsExtractionRecord, FactsExtractionValidationError> {
    let Some(object) = value.as_object() else {
        return Err(invalid(index, "record must be an object"));
    };
    let text = non_empty_str(object.get("text"))
        .ok_or_else(|| invalid(index, "text must be non-empty"))?;
    let date = object
        .get("date")
        .and_then(|value| value.as_str())
        .and_then(valid_date)
        .ok_or_else(|| invalid(index, "date must be YYYY-MM-DD"))?;
    let receipt = object
        .get("receipt")
        .map(|value| {
            ClaimReceipt::parse(value)
                .ok_or_else(|| invalid(index, "receipt is malformed"))
                .map(|_| value.clone())
        })
        .transpose()?;

    let scope = object
        .get("scope")
        .and_then(|value| value.as_str())
        .ok_or_else(|| invalid(index, "scope must be person or project"))?;

    match scope {
        "project" => {
            if object.contains_key("person") {
                return Err(invalid(index, "project record must not carry person"));
            }
            assert_allowed_keys(object, &PROJECT_KEYS, index)?;
            Ok(FactsExtractionRecord::Project {
                text,
                date,
                receipt,
            })
        }
        "person" => {
            if !object.contains_key("person") {
                return Err(invalid(index, "person record requires person"));
            }
            assert_allowed_keys(object, &PERSON_KEYS, index)?;
            let Some(person_object) = object.get("person").and_then(|value| value.as_object())
            else {
                return Err(invalid(index, "person must be an object"));
            };
            assert_allowed_keys(person_object, &PERSON_REFERENCE_KEYS, index)?;
            let name = non_empty_str(person_object.get("name"))
                .ok_or_else(|| invalid(index, "person requires name and aliases"))?;
            let alias_values = person_object
                .get("aliases")
                .and_then(|value| value.as_array())
                .ok_or_else(|| invalid(index, "person requires name and aliases"))?;
            let mut aliases = Vec::with_capacity(alias_values.len());
            for alias in alias_values {
                aliases.push(
                    non_empty_str(Some(alias)).ok_or_else(|| {
                        invalid(index, "person aliases must be non-empty strings")
                    })?,
                );
            }
            Ok(FactsExtractionRecord::Person {
                person: FactsPersonReference { name, aliases },
                text,
                date,
                receipt,
            })
        }
        _ => Err(invalid(index, "scope must be person or project")),
    }
}

/// Rejects rather than ignores: an unknown key means the model's contract and ours disagree, and storing the
/// record would silently drop whatever the extra key was meant to say.
fn assert_allowed_keys(
    object: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    index: usize,
) -> Result<(), FactsExtractionValidationError> {
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            return Err(invalid(index, format!("unexpected field: {key}")));
        }
    }
    Ok(())
}

/// A real calendar date, not merely a `YYYY-MM-DD`-shaped string.
///
/// `2026-02-30` parses as numbers but is not a day, and a fact filed under it would land in a month note that
/// no real date can ever reference again.
fn valid_date(value: &str) -> Option<String> {
    if value.len() != 10 {
        return None;
    }
    let bytes = value.as_bytes();
    if bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    // `chrono` accepts single-digit months and days, so the shape check above is what keeps the format exact.
    if !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return None;
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .map(|_| value.to_owned())
}

fn non_empty_str(value: Option<&serde_json::Value>) -> Option<String> {
    let trimmed = value.and_then(|value| value.as_str())?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn invalid(index: usize, message: impl std::fmt::Display) -> FactsExtractionValidationError {
    FactsExtractionValidationError {
        message: format!("facts extraction line {}: {message}", index + 1),
    }
}

/// A batch id the recovery envelope may be named after: UUID v4 in canonical 8-4-4-4-12 form.
///
/// Uppercase hex is accepted, because a model that upper-cases its id still produced a unique id; only the
/// shape and the version/variant bits are contractual.
pub fn is_uuid_v4(batch_id: &str) -> bool {
    let bytes = batch_id.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let is_dash = matches!(index, 8 | 13 | 18 | 23);
        if is_dash != (*byte == b'-') {
            return false;
        }
    }
    bytes
        .iter()
        .enumerate()
        .filter(|(index, _)| !matches!(index, 8 | 13 | 18 | 23))
        .all(|(_, byte)| byte.is_ascii_hexdigit())
        && bytes[14] == b'4'
        && matches!(bytes[19], b'8' | b'9' | b'a' | b'b' | b'A' | b'B')
}

/// The hash a recovery envelope pins, so an applied batch can be proven to match the one that was planned.
///
/// Agreement records join the hash only when present: batches without them keep the hash they had before
/// agreements existed, so an in-flight recovery envelope written by an older build still validates.
/// The hash a recovery envelope pins, so an applied batch can be proven to match the one that was planned.
///
/// An empty agreement list hashes the same as no agreements at all, and that shape is a bare array — not an
/// object wrapping one. Both details are load-bearing: a recovery envelope written before agreements existed
/// must still validate against a batch re-read after an upgrade.
pub fn records_hash(records: &[FactsExtractionRecord], agreements: &[serde_json::Value]) -> String {
    let json = if agreements.is_empty() {
        serde_json::to_string(records)
    } else {
        serde_json::to_string(&serde_json::json!({ "records": records, "agreements": agreements }))
    }
    .unwrap_or_default();
    crate::store::sha256_hex(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receipt_json() -> serde_json::Value {
        serde_json::json!({ "source": "agent" })
    }

    fn project(text: &str, date: &str) -> serde_json::Value {
        serde_json::json!({ "scope": "project", "text": text, "date": date })
    }

    fn person(name: &str, aliases: serde_json::Value, text: &str) -> serde_json::Value {
        serde_json::json!({
            "scope": "person",
            "person": { "name": name, "aliases": aliases },
            "text": text,
            "date": "2026-11-01"
        })
    }

    #[test]
    fn a_project_record_parses() {
        let record =
            parse_facts_extraction_record(&project("likes tea", "2026-11-01"), 0).expect("parses");
        assert_eq!(record.text(), "likes tea");
        assert_eq!(record.date(), "2026-11-01");
        assert_eq!(record.person(), None);
    }

    #[test]
    fn text_is_trimmed_but_kept_whole_inside() {
        let record =
            parse_facts_extraction_record(&project("  likes  tea  ", "2026-11-01"), 0).expect("ok");
        assert_eq!(record.text(), "likes  tea");
    }

    #[test]
    fn a_person_record_parses_with_its_aliases() {
        let record = parse_facts_extraction_record(
            &person("Indosaram", serde_json::json!(["Indo"]), "owns the repo"),
            0,
        )
        .expect("parses");
        let person = record.person().expect("person");
        assert_eq!(person.name, "Indosaram");
        assert_eq!(person.aliases, vec!["Indo".to_owned()]);
    }

    #[test]
    fn a_person_with_no_aliases_is_allowed() {
        let record =
            parse_facts_extraction_record(&person("A", serde_json::json!([]), "t"), 0).expect("ok");
        assert_eq!(
            record.person().expect("person").aliases,
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_empty_text_is_refused() {
        for text in ["", "   "] {
            let error = parse_facts_extraction_record(&project(text, "2026-11-01"), 0)
                .expect_err("refused");
            assert!(error.message.contains("text must be non-empty"), "{error}");
        }
    }

    #[test]
    fn a_missing_scope_is_refused() {
        let error = parse_facts_extraction_record(
            &serde_json::json!({ "text": "t", "date": "2026-11-01" }),
            0,
        )
        .expect_err("refused");
        assert!(
            error.message.contains("scope must be person or project"),
            "{error}"
        );
    }

    #[test]
    fn an_unknown_scope_is_refused() {
        for scope in ["team", "Project", ""] {
            let error = parse_facts_extraction_record(
                &serde_json::json!({ "scope": scope, "text": "t", "date": "2026-11-01" }),
                0,
            )
            .expect_err("refused");
            assert!(
                error.message.contains("scope must be person or project"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_project_record_carrying_a_person_is_refused() {
        let mut value = project("t", "2026-11-01");
        value["person"] = serde_json::json!({ "name": "A", "aliases": [] });
        let error = parse_facts_extraction_record(&value, 0).expect_err("refused");
        assert!(error.message.contains("must not carry person"), "{error}");
    }

    #[test]
    fn a_person_record_without_a_person_is_refused() {
        let error = parse_facts_extraction_record(
            &serde_json::json!({ "scope": "person", "text": "t", "date": "2026-11-01" }),
            0,
        )
        .expect_err("refused");
        assert!(error.message.contains("requires person"), "{error}");
    }

    #[test]
    fn an_extra_record_field_is_refused() {
        let mut value = project("t", "2026-11-01");
        value["confidence"] = serde_json::json!(0.9);
        let error = parse_facts_extraction_record(&value, 0).expect_err("refused");
        assert!(
            error.message.contains("unexpected field: confidence"),
            "{error}"
        );
    }

    #[test]
    fn an_extra_person_field_is_refused() {
        let mut value = person("A", serde_json::json!([]), "t");
        value["person"]["slug"] = serde_json::json!("a");
        let error = parse_facts_extraction_record(&value, 0).expect_err("refused");
        assert!(error.message.contains("unexpected field: slug"), "{error}");
    }

    #[test]
    fn a_person_without_aliases_is_refused() {
        let error = parse_facts_extraction_record(
            &serde_json::json!({
                "scope": "person",
                "person": { "name": "A" },
                "text": "t",
                "date": "2026-11-01"
            }),
            0,
        )
        .expect_err("refused");
        assert!(
            error.message.contains("requires name and aliases"),
            "{error}"
        );
    }

    #[test]
    fn an_empty_alias_is_refused() {
        let error =
            parse_facts_extraction_record(&person("A", serde_json::json!(["", "B"]), "t"), 0)
                .expect_err("refused");
        assert!(
            error.message.contains("aliases must be non-empty"),
            "{error}"
        );
    }

    #[test]
    fn a_non_object_person_is_refused() {
        let error = parse_facts_extraction_record(
            &serde_json::json!({
                "scope": "person",
                "person": "A",
                "text": "t",
                "date": "2026-11-01"
            }),
            0,
        )
        .expect_err("refused");
        assert!(
            error.message.contains("person must be an object"),
            "{error}"
        );
    }

    #[test]
    fn a_malformed_receipt_is_refused() {
        let mut value = project("t", "2026-11-01");
        value["receipt"] = serde_json::json!({ "source": "guess" });
        let error = parse_facts_extraction_record(&value, 0).expect_err("refused");
        assert!(error.message.contains("receipt is malformed"), "{error}");
    }

    #[test]
    fn a_valid_receipt_is_kept() {
        let mut value = project("t", "2026-11-01");
        value["receipt"] = receipt_json();
        let record = parse_facts_extraction_record(&value, 0).expect("parses");
        assert_eq!(record.receipt(), Some(&receipt_json()));
    }

    #[test]
    fn a_date_that_is_not_a_real_day_is_refused() {
        for date in ["2026-02-30", "2026-13-01", "2026-00-10", "2026-11-31"] {
            let error = parse_facts_extraction_record(&project("t", date), 0).expect_err("refused");
            assert!(
                error.message.contains("date must be YYYY-MM-DD"),
                "{date}: {error}"
            );
        }
    }

    #[test]
    fn a_mis_shaped_date_is_refused() {
        for date in [
            "2026-1-01",
            "26-11-01",
            "2026/11/01",
            "2026-11-01T00:00:00Z",
            "",
            "not-a-date",
        ] {
            assert!(
                parse_facts_extraction_record(&project("t", date), 0).is_err(),
                "date: {date}"
            );
        }
    }

    #[test]
    fn a_leap_day_in_a_leap_year_is_accepted() {
        assert!(parse_facts_extraction_record(&project("t", "2024-02-29"), 0).is_ok());
    }

    #[test]
    fn a_leap_day_in_a_common_year_is_refused() {
        assert!(parse_facts_extraction_record(&project("t", "2026-02-29"), 0).is_err());
    }

    #[test]
    fn the_error_names_the_one_based_line() {
        let error =
            parse_facts_extraction_record(&project("", "2026-11-01"), 2).expect_err("refused");
        assert!(
            error.message.starts_with("facts extraction line 3:"),
            "{error}"
        );
    }

    #[test]
    fn a_bad_date_is_reported_before_a_missing_scope() {
        let error = parse_facts_extraction_record(
            &serde_json::json!({ "text": "t", "date": "2026-02-30" }),
            0,
        )
        .expect_err("refused");
        assert!(error.message.contains("date must be YYYY-MM-DD"), "{error}");
    }

    #[test]
    fn jsonl_parses_each_line_in_order() {
        let raw = format!(
            "{}\n{}\n",
            project("first", "2026-11-01"),
            person("A", serde_json::json!([]), "second")
        );
        let records = parse_facts_extraction_jsonl(&raw).expect("parses");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].text(), "first");
        assert_eq!(records[1].text(), "second");
    }

    #[test]
    fn jsonl_skips_blank_lines() {
        let raw = format!(
            "\n{}\n\n   \n{}\n",
            project("first", "2026-11-01"),
            project("second", "2026-11-02")
        );
        assert_eq!(parse_facts_extraction_jsonl(&raw).expect("parses").len(), 2);
    }

    #[test]
    fn jsonl_reports_the_failing_line_number() {
        let raw = format!("{}\nnot json\n", project("first", "2026-11-01"));
        let error = parse_facts_extraction_jsonl(&raw).expect_err("refused");
        assert!(error.message.contains("line 2"), "{error}");
    }

    #[test]
    fn an_empty_jsonl_document_is_no_records() {
        assert!(parse_facts_extraction_jsonl("").expect("ok").is_empty());
        assert!(
            parse_facts_extraction_jsonl("\n \n")
                .expect("ok")
                .is_empty()
        );
    }

    #[test]
    fn one_bad_record_fails_the_whole_document() {
        let raw = format!(
            "{}\n{}\n",
            project("ok", "2026-11-01"),
            project("", "2026-11-01")
        );
        assert!(parse_facts_extraction_jsonl(&raw).is_err());
    }

    #[test]
    fn a_canonical_uuid_v4_is_accepted() {
        for id in [
            "0f7f5b3c-1a2b-4c3d-8e4f-5a6b7c8d9e0f",
            "9A2B4C6D-1234-4ABC-9DEF-0123456789AB",
        ] {
            assert!(is_uuid_v4(id), "{id}");
        }
    }

    #[test]
    fn a_uuid_of_the_wrong_version_is_refused() {
        assert!(!is_uuid_v4("0f7f5b3c-1a2b-1c3d-8e4f-5a6b7c8d9e0f"));
        assert!(!is_uuid_v4("0f7f5b3c-1a2b-5c3d-8e4f-5a6b7c8d9e0f"));
    }

    #[test]
    fn a_uuid_with_a_bad_variant_is_refused() {
        assert!(!is_uuid_v4("0f7f5b3c-1a2b-4c3d-1e4f-5a6b7c8d9e0f"));
        assert!(!is_uuid_v4("0f7f5b3c-1a2b-4c3d-ce4f-5a6b7c8d9e0f"));
    }

    #[test]
    fn a_mis_shaped_uuid_is_refused() {
        for id in [
            "",
            "not-a-uuid",
            "0f7f5b3c1a2b4c3d8e4f5a6b7c8d9e0f",
            "0f7f5b3c-1a2b-4c3d-8e4f-5a6b7c8d9e0",
            "0f7f5b3c-1a2b-4c3d-8e4f-5a6b7c8d9e0f0",
            "0f7f5b3c_1a2b_4c3d_8e4f_5a6b7c8d9e0f",
            "0f7f5b3c-1a2b-4c3d-8e4f-5a6b7c8d9e0g",
        ] {
            assert!(!is_uuid_v4(id), "{id}");
        }
    }

    #[test]
    fn the_records_hash_is_stable_for_the_same_records() {
        let records = vec![
            parse_facts_extraction_record(&project("a", "2026-11-01"), 0).expect("a"),
            parse_facts_extraction_record(&project("b", "2026-11-02"), 1).expect("b"),
        ];
        assert_eq!(records_hash(&records, &[]), records_hash(&records, &[]));
    }

    #[test]
    fn the_records_hash_changes_when_a_record_changes() {
        let first = vec![parse_facts_extraction_record(&project("a", "2026-11-01"), 0).expect("a")];
        let second =
            vec![parse_facts_extraction_record(&project("b", "2026-11-01"), 0).expect("b")];
        assert_ne!(records_hash(&first, &[]), records_hash(&second, &[]));
    }

    #[test]
    fn the_records_hash_is_order_sensitive() {
        let a = parse_facts_extraction_record(&project("a", "2026-11-01"), 0).expect("a");
        let b = parse_facts_extraction_record(&project("b", "2026-11-01"), 0).expect("b");
        assert_ne!(
            records_hash(&[a.clone(), b.clone()], &[]),
            records_hash(&[b, a], &[])
        );
    }

    #[test]
    fn an_empty_batch_still_hashes() {
        let hash = records_hash(&[], &[]);
        assert_eq!(hash.len(), 64);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn agreements_join_the_hash_only_when_the_list_is_non_empty() {
        let records =
            vec![parse_facts_extraction_record(&project("a", "2026-11-01"), 0).expect("a")];
        let bare = records_hash(&records, &[]);
        let agreement = serde_json::json!({ "text": "never deploy on fridays" });
        assert_ne!(
            bare,
            records_hash(&records, std::slice::from_ref(&agreement))
        );
    }

    #[test]
    fn a_batch_without_agreements_hashes_as_a_bare_array() {
        // Pinned against the pre-agreement shape: an envelope planned before agreements existed must still
        // validate, so the bare case must not be an object wrapping the records.
        let records =
            vec![parse_facts_extraction_record(&project("a", "2026-11-01"), 0).expect("a")];
        let expected = crate::store::sha256_hex(&serde_json::to_string(&records).expect("json"));
        assert_eq!(records_hash(&records, &[]), expected);
    }
}
