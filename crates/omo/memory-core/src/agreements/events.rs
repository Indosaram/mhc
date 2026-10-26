//! Stable extraction receipts, including migration keys for pre-journal ledgers.

use std::collections::BTreeSet;

use super::types::{AgreementExtractionRecord, AgreementItem};
use crate::support::sha256::sha256_hex;

pub fn parse_agreement_event_line(line: &str) -> Option<String> {
    let key = line
        .strip_prefix("<!-- agreement-event: ")?
        .strip_suffix(" -->")?;
    (key.len() == 64
        && key
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| key.to_string())
}

pub fn serialize_agreement_events(keys: &[String]) -> Vec<String> {
    keys.iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|key| format!("<!-- agreement-event: {key} -->"))
        .collect()
}

pub fn agreement_event_key(id: &str, record: &AgreementExtractionRecord) -> String {
    let (date, source) = match record {
        AgreementExtractionRecord::New { date, source, .. }
        | AgreementExtractionRecord::Reaffirm { date, source, .. } => (date, source),
    };
    match source {
        None => legacy_agreement_event_key(id, date),
        Some(source) => digest(&["source-v1", id, &source.session_id, &source.message_id]),
    }
}

pub fn legacy_agreement_event_key(id: &str, date: &str) -> String {
    digest(&["legacy-v1", id, date])
}

pub fn legacy_agreement_events(items: &[AgreementItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| {
            item.last
                .as_deref()
                .map(|last| legacy_agreement_event_key(&item.id, last))
        })
        .collect()
}

fn digest(parts: &[&str]) -> String {
    // JSON string-array encoding is shared with the upstream event-key contract.
    sha256_hex(serde_json::json!(parts).to_string().as_bytes())
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
