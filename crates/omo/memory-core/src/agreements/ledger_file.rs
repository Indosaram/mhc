//! Reading and rendering `system/agreements.md` in the worktree and at a revision.

use std::path::Path;

use crate::git::{GitError, GitMemoryRepo};
use crate::memfs::{FrontmatterError, MemoryFrontmatter, parse_memory_file, render_memory_file};

use super::events::{legacy_agreement_events, serialize_agreement_events};
use super::format::{parse_agreements_ledger, serialize_agreements_ledger};
use super::types::{AGREEMENTS_DESCRIPTION, AGREEMENTS_PATH, AgreementItem, FactsKnownAgreement};

/// The worktree ledger, or `None` when the file does not exist.
pub fn read_agreements_worktree(repo_dir: &Path) -> std::io::Result<Option<String>> {
    match crate::fs::resilient::read_to_string(&repo_dir.join(AGREEMENTS_PATH)) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// The ledger at `revision`, or `None` when that tree has no ledger.
pub fn read_agreements_at(
    repo: &GitMemoryRepo,
    revision: &str,
) -> Result<Option<String>, GitError> {
    if !repo
        .ls_tree(Some(revision), None)?
        .iter()
        .any(|path| path == AGREEMENTS_PATH)
    {
        return Ok(None);
    }
    repo.show(revision, AGREEMENTS_PATH).map(Some)
}

/// Items of a ledger file; an unparseable file reads as empty.
pub fn agreement_items_of(content: Option<&str>) -> Vec<AgreementItem> {
    content
        .and_then(|content| parse_memory_file(content).ok())
        .map(|parsed| parse_agreements_ledger(&parsed.body).items)
        .unwrap_or_default()
}

/// Keeps the existing frontmatter and any line the parser could not read.
pub fn render_agreements_file(
    existing: Option<&str>,
    items: &[AgreementItem],
) -> Result<String, FrontmatterError> {
    render_agreements_file_with_receipts(existing, items, None)
}

pub fn render_agreements_file_with_receipts(
    existing: Option<&str>,
    items: &[AgreementItem],
    receipts: Option<&[String]>,
) -> Result<String, FrontmatterError> {
    let parsed = existing.map(parse_memory_file).transpose()?;
    let ledger = parsed
        .as_ref()
        .map(|file| parse_agreements_ledger(&file.body));
    let prior = ledger
        .as_ref()
        .map(|ledger| ledger.receipts.clone())
        .unwrap_or_default();
    let kept = receipts.map(<[String]>::to_vec).unwrap_or_else(|| {
        if prior.is_empty() {
            legacy_agreement_events(
                ledger
                    .as_ref()
                    .map(|ledger| ledger.items.as_slice())
                    .unwrap_or(&[]),
            )
        } else {
            prior
        }
    });
    let previous = parsed.clone();
    let (frontmatter, mut unparsed) = match parsed {
        None => (
            MemoryFrontmatter {
                description: AGREEMENTS_DESCRIPTION.to_string(),
                read_only: None,
                kind: None,
                aliases: None,
                extra: None,
            },
            Vec::new(),
        ),
        Some(file) => {
            let unparsed = parse_agreements_ledger(&file.body).unparsed;
            (file.frontmatter, unparsed)
        }
    };
    unparsed.extend(serialize_agreement_events(&kept));
    let serialized = serialize_agreements_ledger(items, &unparsed);
    let body = if serialized.is_empty() {
        String::new()
    } else {
        format!("{serialized}\n")
    };
    let frontmatter = crate::tools::claim_frontmatter::stamp_receipt(
        previous.as_ref(),
        &frontmatter,
        &body,
        None,
    );
    render_memory_file(&frontmatter, &body)
}

pub fn known_agreements_of(items: &[AgreementItem]) -> Vec<FactsKnownAgreement> {
    items
        .iter()
        .map(|item| FactsKnownAgreement {
            id: item.id.clone(),
            scope: item.scope,
            by: item.by,
            text: item.text.clone(),
            n: item.n,
        })
        .collect()
}

#[cfg(test)]
#[path = "ledger_file_tests.rs"]
mod tests;
