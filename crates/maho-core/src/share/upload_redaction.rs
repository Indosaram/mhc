//! Port of senpi packages/coding-agent/src/core/share/upload-redaction.ts.

use std::collections::BTreeMap;

use super::share_redaction::{
    ShareRedactionTally, current_share_redaction_context, redact_share_text,
};

/// The redacted text and how many values of each category were removed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRedaction {
    pub text: String,
    pub counts: BTreeMap<String, usize>,
}

/// The redaction for any text an extension sends off this machine. It is the `/share` redactor
/// itself with this machine's identity; `extra_literals` (blank entries ignored) are removed too.
pub fn redact_for_upload(text: &str, extra_literals: &[String]) -> UploadRedaction {
    let mut tally = ShareRedactionTally::default();
    let mut context = current_share_redaction_context();
    let extra: Vec<String> = extra_literals
        .iter()
        .filter(|value| !value.trim().is_empty())
        .cloned()
        .collect();
    if !extra.is_empty() {
        context.extra_literals = extra;
    }
    let redacted = redact_share_text(text, &context, &mut tally);
    let counts = tally
        .iter()
        .map(|(category, n)| (category.as_str().to_owned(), *n))
        .collect();
    UploadRedaction {
        text: redacted,
        counts,
    }
}
