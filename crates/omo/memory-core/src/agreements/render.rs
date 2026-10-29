//! The `<agreements>` projection section of the compiled memory block.

use super::format::{parse_agreements_ledger, sort_agreements};
use super::types::{AGREEMENTS_PATH, AgreementItem, AgreementStatus};

const PREAMBLE: &str = "Standing agreements with your person: [scope] [author] [broken xN] text. NEEDS-HUMAN means it was broken past the threshold: do not break it again; if the task requires it, stop and ask.";

/// Empty string when the ledger holds no parseable agreement.
pub fn render_agreements_section(body: &str) -> String {
    let items = parse_agreements_ledger(body).items;
    if items.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "<agreements>".to_string(),
        format!("<projection>$MEMORY_DIR/{AGREEMENTS_PATH}</projection>"),
        PREAMBLE.to_string(),
    ];
    lines.extend(sort_agreements(&items).iter().map(render_line));
    lines.push("</agreements>".to_string());
    lines.join("\n")
}

fn render_line(item: &AgreementItem) -> String {
    let flag = if item.status == AgreementStatus::NeedsHuman {
        " NEEDS-HUMAN:"
    } else {
        ""
    };
    format!(
        "- [{}] [{}] [broken x{}]{flag} {}",
        item.scope.as_str(),
        item.by.as_str(),
        item.n,
        item.text
    )
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
