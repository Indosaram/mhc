//! Canonical fact receipt presentation and legacy suffix removal.
use super::extraction::FactsExtractionRecord;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "lowercase", deny_unknown_fields)]
pub enum ClaimReceipt {
    User {
        #[serde(rename = "userQuote")]
        user_quote: String,
        approves: String,
    },
    Agent {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        downgraded: Option<bool>,
    },
    Inferred,
}

pub fn parse_claim_receipt(value: &serde_json::Value) -> Option<ClaimReceipt> {
    crate::provenance::claim::parse_claim_receipt(value)?;
    let obj = value.as_object()?;
    if obj
        .get("downgraded")
        .is_some_and(|value| value != &serde_json::Value::Bool(true))
    {
        return None;
    }
    let receipt: ClaimReceipt = serde_json::from_value(value.clone()).ok()?;
    match &receipt {
        ClaimReceipt::User {
            user_quote,
            approves,
        } if user_quote.trim().is_empty()
            || approves.len() != 16
            || !approves
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            None
        }
        ClaimReceipt::Agent {
            downgraded: Some(false),
        } => None,
        _ => Some(receipt),
    }
}

pub fn render_claim_receipt(receipt: &ClaimReceipt) -> String {
    match receipt {
        ClaimReceipt::User { user_quote, approves } => {
            let line = user_quote.split_whitespace().collect::<Vec<_>>().join(" ");
            let quote = if line.encode_utf16().count() > 300 { format!("{}...", String::from_utf16_lossy(&line.encode_utf16().take(300).collect::<Vec<_>>())) } else { line };
            format!("user replied \"{quote}\" to proposal {approves}; a record, not the user's approval")
        }
        ClaimReceipt::Agent { downgraded: Some(true) } => "agent suggestion, not a user decision (claimed as the user's, but no matching user words)".into(),
        ClaimReceipt::Agent { .. } => "agent suggestion, not a user decision".into(),
        ClaimReceipt::Inferred => "inferred, not a user decision".into(),
    }
}

pub fn fact_display_text(record: &FactsExtractionRecord) -> String {
    let text = strip_receipt_suffix(record.text());
    match record.receipt() {
        Some(value) => match parse_claim_receipt(value) {
            Some(receipt) => format!("{text} (source: {})", render_claim_receipt(&receipt)),
            None => text,
        },
        None => text,
    }
}

pub fn strip_receipt_suffix(content: &str) -> String {
    let pattern = r#"\s*\(source: (?:user replied "[^"\n]*" to proposal [0-9a-f]{16}; a record, not the user's approval|user said "[^"\n]*"(?: approving proposal [0-9a-f]{16})?|agent suggestion[^()\n]*(?:\([^()\n]*\)[^()\n]*)*|inferred[^()\n]*|mixed[^()\n]*)\)"#;
    regex::Regex::new(pattern)
        .expect("receipt pattern")
        .replace_all(content, "")
        .trim()
        .to_string()
}

pub fn stamp_receipt(previous: Option<&str>, rendered: String) -> String {
    crate::tools::claim_frontmatter::restamp_content(previous, &rendered, None)
        .expect("rendered memory frontmatter")
}
pub fn stripped_legacy_user_header(before: Option<&str>, after: &str) -> bool {
    crate::tools::claim_frontmatter::stripped_legacy_user_header(before, after)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_validation_and_suffixes() {
        assert!(parse_claim_receipt(&serde_json::json!({"source":"user","userQuote":"yes","approves":"0123456789abcdef"})).is_some());
        for value in [
            serde_json::json!({"source":"user","userQuote":"yes"}),
            serde_json::json!({"source":"agent","downgraded":false}),
            serde_json::json!({"source":"inferred","extra":true}),
        ] {
            assert!(parse_claim_receipt(&value).is_none());
        }
        assert_eq!(
            strip_receipt_suffix(
                "fact (source: agent suggestion, not a user decision (claimed as the user's, but no matching user words))"
            ),
            "fact"
        );
        assert_eq!(
            strip_receipt_suffix(
                "fact (source: user said \"yes\" approving proposal 0123456789abcdef)"
            ),
            "fact"
        );
    }
    #[test]
    fn legacy_header_removed_and_user_record_mixed() {
        let rendered = "---\ndescription: fact\n---\nnew\n".to_string();
        assert_eq!(
            stamp_receipt(
                Some("---\ndescription: fact\nprovenance: user said yes\n---\n"),
                rendered.clone()
            ),
            rendered
        );
        assert!(
            stamp_receipt(
                Some("---\ndescription: fact\nprovenance: user replied yes\n---\n"),
                rendered
            )
            .contains("provenance: \"mixed:")
        );
    }
}
