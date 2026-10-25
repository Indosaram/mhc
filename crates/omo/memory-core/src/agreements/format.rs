//! Parse and serialize the agreements ledger body.

use std::collections::HashSet;

use crate::facts::person_routing::normalize_observation_text;
use crate::support::sha256::sha256_hex;

use super::events::parse_agreement_event_line;
use super::types::{AgreementAuthor, AgreementItem, AgreementScope, AgreementStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgreementsLedgerParse {
    pub items: Vec<AgreementItem>,
    pub diagnostics: Vec<String>,
    pub receipts: Vec<String>,
    pub unparsed: Vec<String>,
}

pub fn normalize_agreement_text(text: &str) -> String {
    normalize_observation_text(text)
}

/// Stable id: first 10 hex characters of the sha256 of the normalized text.
pub fn agreement_id(text: &str) -> String {
    let digest = sha256_hex(normalize_agreement_text(text).as_bytes());
    digest.chars().take(10).collect()
}

/// A ledger line split into scope, text and optional metadata comment.
struct LedgerLine<'a> {
    scope: AgreementScope,
    text: &'a str,
    meta: &'a str,
}

/// Mirrors `^-\s+\[(global|project)\]\s+(.+?)\s*(?:<!--\s*(.*?)\s*-->)?\s*$` on a trimmed line.
fn split_line(line: &str) -> Option<LedgerLine<'_>> {
    let rest = line.strip_prefix('-')?;
    let rest = strip_leading_space(rest)?;
    let (scope, rest) = if let Some(rest) = rest.strip_prefix("[global]") {
        (AgreementScope::Global, rest)
    } else {
        (AgreementScope::Project, rest.strip_prefix("[project]")?)
    };
    let rest = strip_leading_space(rest)?.trim_end();
    if rest.is_empty() {
        return None;
    }
    let mut from = rest.chars().next().map_or(0, char::len_utf8);
    while let Some(found) = rest.get(from..).and_then(|tail| tail.find("<!--")) {
        let at = from + found;
        let inner = &rest[at + 4..];
        if let Some(meta) = inner.strip_suffix("-->") {
            return Some(LedgerLine {
                scope,
                text: rest[..at].trim_end(),
                meta: meta.trim(),
            });
        }
        from = at + 1;
    }
    Some(LedgerLine {
        scope,
        text: rest,
        meta: "",
    })
}

/// `\s+` at the start: at least one whitespace character, all consumed.
fn strip_leading_space(text: &str) -> Option<&str> {
    let trimmed = text.trim_start();
    (trimmed.len() < text.len()).then_some(trimmed)
}

pub fn parse_agreements_ledger(body: &str) -> AgreementsLedgerParse {
    let mut items: Vec<AgreementItem> = Vec::new();
    let mut diagnostics = Vec::new();
    let mut unparsed = Vec::new();
    let mut receipts = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for (index, raw) in body.split('\n').enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(receipt) = parse_agreement_event_line(line) {
            if !receipts.contains(&receipt) {
                receipts.push(receipt);
            }
            continue;
        }
        let parsed = split_line(line).filter(|found| !found.text.trim().is_empty());
        let Some(found) = parsed else {
            diagnostics.push(format!(
                "line {}: expected \"- [global|project] <agreement>\"",
                index + 1
            ));
            unparsed.push(line.to_string());
            continue;
        };
        let item = match parse_meta(found.meta, found.text.trim(), found.scope) {
            Ok(item) => item,
            Err(problem) => {
                diagnostics.push(format!("line {}: {problem}", index + 1));
                unparsed.push(line.to_string());
                continue;
            }
        };
        if !seen.insert(item.id.clone()) {
            diagnostics.push(format!(
                "line {}: duplicate agreement id {}",
                index + 1,
                item.id
            ));
            unparsed.push(line.to_string());
            continue;
        }
        items.push(item);
    }
    AgreementsLedgerParse {
        items,
        diagnostics,
        receipts,
        unparsed,
    }
}

pub fn serialize_agreements_ledger(items: &[AgreementItem], unparsed: &[String]) -> String {
    let mut lines: Vec<String> = sort_agreements(items).iter().map(serialize_item).collect();
    lines.extend(unparsed.iter().cloned());
    lines.join("\n")
}

/// Human items first, then oldest `since`, then id.
pub fn sort_agreements(items: &[AgreementItem]) -> Vec<AgreementItem> {
    let mut sorted = items.to_vec();
    sorted.sort_by(|a, b| {
        author_rank(a.by)
            .cmp(&author_rank(b.by))
            .then_with(|| {
                a.since
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.since.as_deref().unwrap_or(""))
            })
            .then_with(|| a.id.cmp(&b.id))
    });
    sorted
}

fn serialize_item(item: &AgreementItem) -> String {
    let mut meta = vec![
        format!("id: {}", item.id),
        format!("by: {}", item.by.as_str()),
        format!("n: {}", item.n),
    ];
    if let Some(since) = &item.since {
        meta.push(format!("since: {since}"));
    }
    if let Some(last) = &item.last {
        meta.push(format!("last: {last}"));
    }
    meta.push(format!("status: {}", item.status.as_str()));
    format!(
        "- [{}] {} <!-- {} -->",
        item.scope.as_str(),
        item.text,
        meta.join("; ")
    )
}

fn author_rank(by: AgreementAuthor) -> u8 {
    match by {
        AgreementAuthor::Human => 0,
        AgreementAuthor::Agent => 1,
    }
}

fn is_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, byte)| {
            if index == 4 || index == 7 {
                *byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

fn is_id(value: &str) -> bool {
    value.len() == 10
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `Number(text)` restricted to the safe non-negative integers the ledger accepts.
fn parse_count(raw: &str) -> Option<u64> {
    if raw.is_empty() {
        return Some(0);
    }
    let value: f64 = raw.parse().ok()?;
    let safe = value.is_finite()
        && value.fract() == 0.0
        && (0.0..=9_007_199_254_740_991.0).contains(&value);
    safe.then_some(value as u64)
}

fn parse_meta(raw: &str, text: &str, scope: AgreementScope) -> Result<AgreementItem, String> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for segment in raw.split(';') {
        if segment.trim().is_empty() {
            continue;
        }
        let at = match segment.find(':') {
            Some(at) if at > 0 => at,
            _ => {
                return Err(format!(
                    "metadata segment \"{}\" is not key: value",
                    segment.trim()
                ));
            }
        };
        let key = segment[..at].trim().to_string();
        let value = segment[at + 1..].trim().to_string();
        fields.retain(|(existing, _)| *existing != key);
        fields.push((key, value));
    }
    let get = |key: &str| {
        fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    };
    let id = get("id").map_or_else(|| agreement_id(text), str::to_string);
    if !is_id(&id) {
        return Err("id must be 10 lowercase hex characters".to_string());
    }
    let by = match get("by").unwrap_or("agent") {
        "human" => AgreementAuthor::Human,
        "agent" => AgreementAuthor::Agent,
        _ => return Err("by must be human or agent".to_string()),
    };
    let n = parse_count(get("n").unwrap_or("0"))
        .ok_or_else(|| "n must be a non-negative integer".to_string())?;
    let status = match get("status").unwrap_or("active") {
        "active" => AgreementStatus::Active,
        "needs-human" => AgreementStatus::NeedsHuman,
        _ => return Err("status must be active or needs-human".to_string()),
    };
    let since = get("since");
    let last = get("last");
    if since.is_some_and(|value| !is_date(value)) {
        return Err("since must be YYYY-MM-DD".to_string());
    }
    if last.is_some_and(|value| !is_date(value)) {
        return Err("last must be YYYY-MM-DD".to_string());
    }
    Ok(AgreementItem {
        id,
        scope,
        by,
        text: text.to_string(),
        n,
        since: since.map(str::to_string),
        last: last.map(str::to_string),
        status,
    })
}

#[cfg(test)]
#[path = "format_tests.rs"]
mod tests;
