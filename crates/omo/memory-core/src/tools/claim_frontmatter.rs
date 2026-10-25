//! Provenance claim receipts and legacy frontmatter header migration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::memfs::{
    FrontmatterError, MemoryFrontmatter, ParsedMemoryFile, parse_memory_file, render_memory_file,
};
use crate::memfs::frontmatter_entries::{look_alike_frontmatter_keys, normalized_frontmatter_key};

pub const PROVENANCE_FRONTMATTER_KEY: &str = "provenance";
pub const MIXED_PROVENANCE: &str = "mixed: parts of this note are not the user's recorded words";
pub const PROVENANCE_STRIPPED_TRAILER: &str = "Omo-Provenance-Stripped";

const LEGACY_USER_PREFIX: &str = "user said ";

/// Only the harness may supply this receipt; note contents never establish a claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase", deny_unknown_fields)]
pub enum ClaimReceipt {
    User {
        #[serde(rename = "userQuote")]
        user_quote: String,
        approves: String,
    },
    Agent {
        #[serde(default, skip_serializing_if = "is_false")]
        downgraded: bool,
    },
    Inferred,
}

pub fn parse_claim_receipt(value: &serde_json::Value) -> Option<ClaimReceipt> {
    crate::provenance::claim::parse_claim_receipt(value)?;
    serde_json::from_value(value.clone()).ok()
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn single_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.encode_utf16().count() > 300 {
        format!(
            "{}...",
            String::from_utf16_lossy(&line.encode_utf16().take(300).collect::<Vec<_>>())
        )
    } else {
        line
    }
}

pub fn claim_receipt_trailers(receipt: &ClaimReceipt) -> Vec<String> {
    match receipt {
        ClaimReceipt::User {
            user_quote,
            approves,
        } => vec![
            "Omo-Claim-Source: user".into(),
            format!("Omo-User-Quote: {}", single_line(user_quote)),
            format!("Omo-Claim-Approves: {approves}"),
        ],
        ClaimReceipt::Agent { downgraded } => {
            let mut result = vec!["Omo-Claim-Source: agent".into()];
            if *downgraded {
                result.push("Omo-Claim-Downgraded: unverified-user-quote".into());
            }
            result
        }
        ClaimReceipt::Inferred => vec!["Omo-Claim-Source: inferred".into()],
    }
}

/// Render a claim receipt into its frontmatter text description.
pub fn render_claim_receipt(receipt: &ClaimReceipt) -> String {
    match receipt {
        ClaimReceipt::User {
            user_quote,
            approves,
        } => {
            let quote = single_line(user_quote);
            format!("user replied \"{quote}\" to proposal {approves}; a record, not the user's approval")
        }
        ClaimReceipt::Agent { downgraded: true } => {
            "agent suggestion, not a user decision (claimed as the user's, but no matching user words)"
                .to_string()
        }
        ClaimReceipt::Agent { downgraded: false } => {
            "agent suggestion, not a user decision".to_string()
        }
        ClaimReceipt::Inferred => "inferred, not a user decision".to_string(),
    }
}

/// Stamp or update the provenance receipt in note frontmatter.
pub fn stamp_receipt(
    previous: Option<&ParsedMemoryFile>,
    next: &MemoryFrontmatter,
    body: &str,
    claim: Option<&ClaimReceipt>,
) -> MemoryFrontmatter {
    let prior_header = previous.and_then(|p| read_header(&p.frontmatter));
    let prior = live_header(prior_header.as_deref());
    let re_render = previous.is_some_and(|p| p.body == body && same_fields(&p.frontmatter, next));
    let header: Option<String> = match claim {
        None => {
            if re_render {
                prior.map(str::to_string)
            } else {
                edit_header(prior)
            }
        }
        Some(c) => Some(claim_header(prior, &render_claim_receipt(c))),
    };

    let mut rest = BTreeMap::new();
    if let Some(extra) = &next.extra {
        for (k, v) in extra {
            if normalized_frontmatter_key(k) != PROVENANCE_FRONTMATTER_KEY {
                rest.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(h) = header {
        let json_val = serde_json::to_string(&h).unwrap_or_else(|_| format!("\"{h}\""));
        rest.insert(PROVENANCE_FRONTMATTER_KEY.to_string(), json_val);
    }

    let extra = if rest.is_empty() { None } else { Some(rest) };
    MemoryFrontmatter {
        description: next.description.clone(),
        read_only: next.read_only.clone(),
        kind: next.kind.clone(),
        aliases: next.aliases.clone(),
        extra,
    }
}

/// Re-stamp raw markdown note content with claim receipt rules.
pub fn restamp_content(
    previous: Option<&str>,
    next: &str,
    claim: Option<&ClaimReceipt>,
) -> Result<String, FrontmatterError> {
    let parsed = parse_memory_file(next)?;
    let prev_parsed = previous.map(parse_memory_file).transpose()?;
    let stamped = stamp_receipt(
        prev_parsed.as_ref(),
        &parsed.frontmatter,
        &parsed.body,
        claim,
    );
    let raw_look_alike = !look_alike_frontmatter_keys(next, PROVENANCE_FRONTMATTER_KEY).is_empty();
    let unchanged = !raw_look_alike && stamped.extra == parsed.frontmatter.extra;
    if unchanged {
        Ok(next.to_string())
    } else {
        render_memory_file(&stamped, &parsed.body)
    }
}

/// True if header begins with the legacy user prefix.
pub fn is_legacy_user_header(header: Option<&str>) -> bool {
    header
        .map(|h| h.starts_with(LEGACY_USER_PREFIX))
        .unwrap_or(false)
}

/// True when before had a legacy user header and after does not.
pub fn stripped_legacy_user_header(before: Option<&str>, after: &str) -> bool {
    is_legacy_user_header(header_of(before).as_deref())
        && !is_legacy_user_header(header_of(Some(after)).as_deref())
}

/// Create git commit trailer lines for stripped provenance paths.
pub fn provenance_stripped_trailers(paths: &[String]) -> Vec<String> {
    use std::collections::BTreeSet;
    let mut seen = BTreeSet::new();
    let mut trailers = Vec::new();
    for p in paths {
        if seen.insert(p) {
            trailers.push(format!("{PROVENANCE_STRIPPED_TRAILER}: {p}"));
        }
    }
    trailers
}

/// True if note content carries a legacy user header.
pub fn has_legacy_user_header(content: Option<&str>) -> bool {
    is_legacy_user_header(header_of(content).as_deref())
}

/// Read the string provenance header value if present.
pub fn read_header(frontmatter: &MemoryFrontmatter) -> Option<String> {
    let raw = frontmatter
        .extra
        .as_ref()?
        .get(PROVENANCE_FRONTMATTER_KEY)?;
    if let Ok(serde_json::Value::String(s)) = serde_json::from_str(raw) {
        Some(s)
    } else {
        Some(raw.clone())
    }
}

/// Extract provenance header string from raw markdown content.
pub fn header_of(content: Option<&str>) -> Option<String> {
    let content = content?;
    parse_memory_file(content)
        .ok()
        .and_then(|p| read_header(&p.frontmatter))
}

fn same_fields(a: &MemoryFrontmatter, b: &MemoryFrontmatter) -> bool {
    if a.description != b.description
        || a.read_only != b.read_only
        || a.kind != b.kind
        || a.aliases != b.aliases
    {
        return false;
    }
    let filter_extra = |extra: Option<&BTreeMap<String, String>>| -> BTreeMap<String, String> {
        let mut res = BTreeMap::new();
        if let Some(extra) = extra {
            for (k, v) in extra {
                if normalized_frontmatter_key(k) != PROVENANCE_FRONTMATTER_KEY {
                    res.insert(k.clone(), v.clone());
                }
            }
        }
        res
    };
    filter_extra(a.extra.as_ref()) == filter_extra(b.extra.as_ref())
}

fn live_header(header: Option<&str>) -> Option<&str> {
    if is_legacy_user_header(header) {
        None
    } else {
        header
    }
}

fn is_user_record(header: Option<&str>) -> bool {
    header
        .map(|h| h.starts_with("user replied "))
        .unwrap_or(false)
}

fn edit_header(prior: Option<&str>) -> Option<String> {
    if is_user_record(prior) {
        Some(MIXED_PROVENANCE.to_string())
    } else {
        prior.map(str::to_string)
    }
}

fn claim_header(prior: Option<&str>, rendered: &str) -> String {
    if prior.is_none() || is_user_record(prior) == is_user_record(Some(rendered)) {
        rendered.to_string()
    } else {
        MIXED_PROVENANCE.to_string()
    }
}

#[cfg(test)]
#[path = "claim_frontmatter_tests.rs"]
mod tests;
