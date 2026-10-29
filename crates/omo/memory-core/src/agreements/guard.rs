//! Rules that keep automation from rewriting what the person wrote or resetting counts.

use std::collections::HashMap;

use crate::memfs::parse_memory_file;

use super::format::parse_agreements_ledger;
use super::types::{AgreementAuthor, AgreementItem, AgreementStatus};

/// Items of a ledger file; an unreadable file reads as empty.
pub fn read_agreements_body(content: Option<&str>) -> Vec<AgreementItem> {
    let Some(content) = content else {
        return Vec::new();
    };
    match parse_memory_file(content) {
        Ok(parsed) => parse_agreements_ledger(&parsed.body).items,
        Err(_) => Vec::new(),
    }
}

/// Automation (the memory tools, reflection, dream) may count and reword agent items but must never
/// create, change, or remove an item the person wrote. Returns the reason a write breaks that rule.
pub fn describe_agreements_tool_violation(
    before: Option<&str>,
    after: Option<&str>,
) -> Option<String> {
    let mut prior_receipts = before
        .and_then(|content| parse_memory_file(content).ok())
        .map(|file| parse_agreements_ledger(&file.body).receipts)
        .unwrap_or_default();
    let items_before = read_agreements_body(before);
    let human_before: Vec<&AgreementItem> = items_before
        .iter()
        .filter(|item| item.by == AgreementAuthor::Human)
        .collect();
    let Some(after) = after else {
        if !prior_receipts.is_empty() {
            return Some("agreement event receipts cannot be removed by memory tools".to_string());
        }
        if !human_before.is_empty() {
            return Some("system/agreements.md holds agreements your person wrote; only they can remove them (/agreements drop)".to_string());
        }
        if let Some(flagged) = items_before
            .iter()
            .find(|item| item.status == AgreementStatus::NeedsHuman)
        {
            return Some(flagged_drop(&flagged.id));
        }
        return items_before
            .iter()
            .find(|item| item.n > 0)
            .map(counted_removal);
    };
    let parsed = match parse_memory_file(after) {
        Ok(file) => parse_agreements_ledger(&file.body),
        Err(error) => {
            return Some(format!(
                "system/agreements.md is not a valid memory file: {error}"
            ));
        }
    };
    if !parsed.diagnostics.is_empty() {
        return Some(format!(
            "system/agreements.md is malformed: {}",
            parsed.diagnostics.join("; ")
        ));
    }
    prior_receipts.sort();
    let mut after_receipts = parsed.receipts.clone();
    after_receipts.sort();
    if prior_receipts != after_receipts {
        return Some("agreement event receipts are maintained by the facts extractor and cannot be edited by memory tools".to_string());
    }
    if let Some(counted) = describe_count_tampering(&items_before, &parsed.items) {
        return Some(counted);
    }
    let mut human_after: Vec<&AgreementItem> = parsed
        .items
        .iter()
        .filter(|item| item.by == AgreementAuthor::Human)
        .collect();
    for item in human_before {
        let Some(position) = human_after.iter().position(|kept| kept.id == item.id) else {
            return Some(format!(
                "agreement {} was written by your person and cannot be removed by automation",
                item.id
            ));
        };
        let kept = human_after[position];
        if kept.text != item.text || kept.scope != item.scope {
            return Some(format!(
                "agreement {} was written by your person and cannot be reworded by automation",
                item.id
            ));
        }
        human_after.remove(position);
    }
    human_after.first().map(|forged| {
        format!(
            "agreement {} is marked by: human, which only /agreements add may write",
            forged.id
        )
    })
}

fn describe_count_tampering(before: &[AgreementItem], after: &[AgreementItem]) -> Option<String> {
    let by_id: HashMap<&str, &AgreementItem> =
        after.iter().map(|item| (item.id.as_str(), item)).collect();
    for item in before {
        let Some(kept) = by_id.get(item.id.as_str()) else {
            if item.status == AgreementStatus::NeedsHuman {
                return Some(flagged_drop(&item.id));
            }
            if item.n > 0 {
                return Some(counted_removal(item));
            }
            continue;
        };
        if kept.n < item.n {
            return Some(format!(
                "agreement {} was broken {} times; automation cannot lower that count (/agreements ack)",
                item.id, item.n
            ));
        }
        if item.status == AgreementStatus::NeedsHuman && kept.status != AgreementStatus::NeedsHuman
        {
            return Some(format!(
                "agreement {} is flagged as needing your person; only they can clear it (/agreements ack)",
                item.id
            ));
        }
    }
    None
}

fn flagged_drop(id: &str) -> String {
    format!(
        "agreement {id} is flagged as needing your person; only they can drop it (/agreements drop)"
    )
}

// Removing a counted item and re-adding it later would restart its count at zero.
fn counted_removal(item: &AgreementItem) -> String {
    format!(
        "agreement {} was broken {} times; automation cannot remove it and restart that count (/agreements drop)",
        item.id, item.n
    )
}

#[cfg(test)]
#[path = "guard_tests.rs"]
mod tests;
