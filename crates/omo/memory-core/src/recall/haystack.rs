//! Normalized recall haystacks shared by the substring scorer and the strategy selector.

use crate::search::query::normalize_text;

use super::provider::RecallDocument;

/// Normalized `description\nbody` for one document.
///
/// The pin's `WeakMap` memo is a pure performance optimization, so the port recomputes instead.
pub fn normalized_haystack(document: &RecallDocument) -> String {
    normalize_text(&format!("{}\n{}", document.description, document.body))
}

#[cfg(test)]
#[path = "haystack_tests.rs"]
mod tests;
