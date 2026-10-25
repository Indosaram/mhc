//! Pure mutations of the agreements ledger: facts-lane records and `/agreements` commands.

use std::collections::HashSet;

use super::events::{agreement_event_key, legacy_agreement_events};
use super::format::{agreement_id, normalize_agreement_text, sort_agreements};
use super::types::{
    AgreementAuthor, AgreementExtractionRecord, AgreementItem, AgreementScope, AgreementStatus,
    AgreementsPolicy,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreementsMutation {
    pub items: Vec<AgreementItem>,
    pub newly_flagged: Vec<String>,
    pub events: Vec<AgreementEvent>,
    pub receipts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgreementEvent {
    Evicted {
        date: String,
        item: AgreementItem,
        for_text: String,
    },
    Refused {
        date: String,
        text: String,
    },
    StaleReaffirm {
        date: String,
        id: String,
    },
}

pub fn apply_agreement_records(
    existing: &[AgreementItem],
    records: &[AgreementExtractionRecord],
    policy: &AgreementsPolicy,
) -> AgreementsMutation {
    apply_agreement_records_with_receipts(existing, records, policy, &[])
}

pub fn apply_agreement_records_with_receipts(
    existing: &[AgreementItem],
    records: &[AgreementExtractionRecord],
    policy: &AgreementsPolicy,
    prior_receipts: &[String],
) -> AgreementsMutation {
    let mut receipts = if prior_receipts.is_empty() {
        legacy_agreement_events(existing)
    } else {
        prior_receipts.to_vec()
    };
    let mut known: HashSet<String> = receipts.iter().cloned().collect();
    let mut items: Vec<AgreementItem> = existing.to_vec();
    let committed: HashSet<String> = existing.iter().map(|item| item.id.clone()).collect();
    let mut flagged: Vec<String> = Vec::new();
    let mut events = Vec::new();

    let bump = |items: &mut Vec<AgreementItem>,
                flagged: &mut Vec<String>,
                at: usize,
                date: &str,
                text: Option<&str>| {
        let Some(found) = items.get(at).cloned() else {
            return;
        };
        let n = found.n + 1;
        let reworded = match text {
            Some(text) if found.by == AgreementAuthor::Agent => text.to_string(),
            _ => found.text.clone(),
        };
        let status = if n >= policy.needs_human_threshold {
            AgreementStatus::NeedsHuman
        } else {
            found.status
        };
        if status == AgreementStatus::NeedsHuman
            && found.status != AgreementStatus::NeedsHuman
            && !flagged.contains(&found.id)
        {
            flagged.push(found.id.clone());
        }
        items[at] = AgreementItem {
            text: reworded,
            n,
            last: Some(date.to_string()),
            status,
            ..found
        };
    };

    for record in records {
        match record {
            AgreementExtractionRecord::Reaffirm { id, date, text, .. } => {
                match items.iter().position(|item| item.id == *id) {
                    None => events.push(AgreementEvent::StaleReaffirm {
                        date: date.clone(),
                        id: id.clone(),
                    }),
                    Some(at) => {
                        if claim(id, record, &mut known, &mut receipts) {
                            bump(&mut items, &mut flagged, at, date, text.as_deref());
                        }
                    }
                }
            }
            AgreementExtractionRecord::New {
                applies,
                text,
                date,
                ..
            } => {
                if let Some(at) = find_agreement(&items, text) {
                    if claim(&items[at].id, record, &mut known, &mut receipts) {
                        bump(&mut items, &mut flagged, at, date, None);
                    }
                    continue;
                }
                if known.contains(&agreement_event_key(&agreement_id(text), record)) {
                    continue;
                }
                let status = if 1 >= policy.needs_human_threshold {
                    AgreementStatus::NeedsHuman
                } else {
                    AgreementStatus::Active
                };
                let created = AgreementItem {
                    id: agreement_id(text),
                    scope: *applies,
                    by: AgreementAuthor::Agent,
                    text: text.clone(),
                    n: 1,
                    since: Some(date.clone()),
                    last: Some(date.clone()),
                    status,
                };
                if items.len() >= policy.max_items {
                    let pool: Vec<AgreementItem> = items
                        .iter()
                        .filter(|item| committed.contains(&item.id))
                        .cloned()
                        .collect();
                    let Some(victim) = eviction_candidate(&pool) else {
                        events.push(AgreementEvent::Refused {
                            date: date.clone(),
                            text: text.clone(),
                        });
                        continue;
                    };
                    items.retain(|item| item.id != victim.id);
                    events.push(AgreementEvent::Evicted {
                        date: date.clone(),
                        item: victim,
                        for_text: text.clone(),
                    });
                }
                if status == AgreementStatus::NeedsHuman && !flagged.contains(&created.id) {
                    flagged.push(created.id.clone());
                }
                claim(&created.id, record, &mut known, &mut receipts);
                items.push(created);
            }
        }
    }
    AgreementsMutation {
        items: sort_agreements(&items),
        newly_flagged: flagged,
        events,
        receipts,
    }
}

fn claim(
    id: &str,
    record: &AgreementExtractionRecord,
    known: &mut HashSet<String>,
    receipts: &mut Vec<String>,
) -> bool {
    let mut ids = vec![id.to_string()];
    if let AgreementExtractionRecord::New { text, .. } = record {
        let alias = agreement_id(text);
        if alias != id {
            ids.push(alias);
        }
    }
    let keys: Vec<String> = ids
        .iter()
        .map(|id| agreement_event_key(id, record))
        .collect();
    let duplicate = keys.iter().any(|key| known.contains(key));
    for key in keys {
        known.insert(key.clone());
        if !receipts.contains(&key) {
            receipts.push(key);
        }
    }
    !duplicate
}

/// At the cap only a committed agent item that was never reaffirmed and is not flagged may go: the oldest one.
fn eviction_candidate(items: &[AgreementItem]) -> Option<AgreementItem> {
    let age = |item: &AgreementItem| -> String {
        item.last
            .clone()
            .or_else(|| item.since.clone())
            .unwrap_or_default()
    };
    items
        .iter()
        .filter(|item| {
            item.by == AgreementAuthor::Agent
                && item.status == AgreementStatus::Active
                && item.n <= 1
        })
        .min_by(|a, b| age(a).cmp(&age(b)).then_with(|| a.id.cmp(&b.id)))
        .cloned()
}

pub fn add_human_agreement(
    existing: &[AgreementItem],
    text: &str,
    scope: AgreementScope,
    date: &str,
) -> Vec<AgreementItem> {
    let at = find_agreement(existing, text);
    let item = match at.and_then(|index| existing.get(index)) {
        None => AgreementItem {
            id: agreement_id(text),
            scope,
            by: AgreementAuthor::Human,
            text: text.to_string(),
            n: 0,
            since: Some(date.to_string()),
            last: None,
            status: AgreementStatus::Active,
        },
        Some(found) => AgreementItem {
            scope,
            by: AgreementAuthor::Human,
            text: text.to_string(),
            ..found.clone()
        },
    };
    let mut rest: Vec<AgreementItem> = existing
        .iter()
        .enumerate()
        .filter(|(index, _)| Some(*index) != at)
        .map(|(_, item)| item.clone())
        .collect();
    rest.push(item);
    sort_agreements(&rest)
}

/// Resets a counted item; `None` when the id is unknown.
pub fn ack_agreement(existing: &[AgreementItem], id: &str) -> Option<Vec<AgreementItem>> {
    if !existing.iter().any(|item| item.id == id) {
        return None;
    }
    Some(
        existing
            .iter()
            .map(|item| {
                if item.id == id {
                    AgreementItem {
                        n: 0,
                        status: AgreementStatus::Active,
                        ..item.clone()
                    }
                } else {
                    item.clone()
                }
            })
            .collect(),
    )
}

/// Removes an item; `None` when the id is unknown.
pub fn drop_agreement(existing: &[AgreementItem], id: &str) -> Option<Vec<AgreementItem>> {
    if !existing.iter().any(|item| item.id == id) {
        return None;
    }
    Some(
        existing
            .iter()
            .filter(|item| item.id != id)
            .cloned()
            .collect(),
    )
}

fn find_agreement(items: &[AgreementItem], text: &str) -> Option<usize> {
    let id = agreement_id(text);
    let normalized = normalize_agreement_text(text);
    items
        .iter()
        .position(|item| item.id == id || normalize_agreement_text(&item.text) == normalized)
}

#[cfg(test)]
#[path = "mutate_tests.rs"]
mod tests;
