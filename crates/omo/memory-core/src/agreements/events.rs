//! Stable agreement event receipts retained after item removal.
use crate::agreements::{AgreementExtractionRecord, AgreementItem};
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
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|key| format!("<!-- agreement-event: {key} -->"))
        .collect()
}
pub fn legacy_agreement_event_key(id: &str, date: &str) -> String {
    sha256_hex(
        serde_json::to_string(&["legacy-v1", id, date])
            .expect("string array")
            .as_bytes(),
    )
}
pub fn agreement_event_key(id: &str, record: &AgreementExtractionRecord) -> String {
    let (date, source) = match record {
        AgreementExtractionRecord::New { date, source, .. }
        | AgreementExtractionRecord::Reaffirm { date, source, .. } => (date, source),
    };
    match source {
        Some(source) => sha256_hex(
            serde_json::to_string(&["source-v1", id, &source.session_id, &source.message_id])
                .expect("string array")
                .as_bytes(),
        ),
        None => legacy_agreement_event_key(id, date),
    }
}
pub fn legacy_agreement_events(items: &[AgreementItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| {
            item.last
                .as_ref()
                .map(|date| legacy_agreement_event_key(&item.id, date))
        })
        .collect()
}
