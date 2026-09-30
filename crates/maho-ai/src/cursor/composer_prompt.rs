//! Port of senpi packages/ai/src/cursor/composer-prompt.ts.
//!
//! Composer 2 and 2.5 are Cursor's continued pretraining plus large-scale agentic RL on top of
//! Moonshot's Kimi K2.5 checkpoint, and Cursor trains them inside essentially the same agent
//! harness it serves them from (arXiv:2603.24477; cursor.com/blog/composer-2-5). The policy is
//! therefore shaped around Cursor's own tool surface, and Cursor reports that a model given an
//! unfamiliar tool or edit format still works but spends more reasoning tokens and makes more
//! mistakes.
//!
//! Driving Composer through this client is exactly that off-distribution case, so the prefix
//! supplies only what the host prompt cannot know: which tools this wire surface actually
//! exposes, and the operating rules for the habits Cursor documents as trained-in. Everything the
//! host prompt already covers (verification depth, output style, commit policy) is deliberately
//! absent: K2-family models follow instructions strictly enough that restating an existing rule
//! buys nothing and dilutes the rules that are new.
//!
//! The wording is positive and procedural rather than a stack of prohibitions. A prohibition
//! invites this family to spend reasoning deciding whether the current case is the prohibited
//! one, while a named tool and a terminal condition install the behavior directly.

use std::sync::LazyLock;

use regex::Regex;

/// The port of the TS `CURSOR_COMPOSER_PROMPT` export.
pub const CURSOR_COMPOSER_PROMPT: &str = "You are running in this client, not in Cursor. Your tools on this surface are read, ls, grep, write, delete, diagnostics, and shell. Tool names from other harnesses are unavailable here.\n\nReach for repository files through the native tools: read for file contents, ls for directory entries, grep for content and filename search, write to create or modify, delete to remove, diagnostics for a file's current errors. These carry line anchors and result metadata that shell output does not.\n\nKeep shell for terminal work: tests, builds, package scripts, git, and process control. Put only the command in the command string.\n\nTool arguments are the schema object the tool declares. Keep explanation and markdown in your message text, where it belongs.\n\nRead a file before you edit it, and read it again after any write before relying on its contents. Copy line anchors from the most recent tool output rather than computing or adjusting them; when an anchor is rejected, re-read and use the anchors that come back.\n\nRun independent searches and reads together in one batch. Keep dependent steps in sequence, and let a step that needs the previous result wait for it.\n\nA task is finished when the requested behavior is in place and you have watched it work: the relevant test, build, or command run, and its output read. Until you have that, keep going. If something remains unproven or broken, say which part and why.\n\nWhen asked a question, answer it from the code and stop there. Edit when a change was requested or when a fix is confirmed.";

/// Composer ids on this provider, e.g. `composer-2.5`, `composer-2.6-thinking-max`,
/// `composer-2.6-lite-medium-fast`. The match is name-based and version-agnostic because the
/// trained-in harness habits belong to the family rather than to any one release, and Cursor
/// serves ids ahead of its public docs.
///
/// Cursor also resells first-party Kimi models (`kimi-k2.7-code`, `kimi-k3-*`). Those are
/// ordinary hosted models rather than Cursor-harness-trained policies, so keying on the Composer
/// name keeps them out.
fn composer_model_id_pattern() -> &'static Regex {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:^|[/:._-])composer(?:[/:._-]|\d|$)").unwrap_or_else(|error| panic!("static composer pattern: {error}"))
    });
    &PATTERN
}

pub fn is_cursor_composer_model(model_id: &str) -> bool {
    composer_model_id_pattern().is_match(model_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Deserialize)]
    struct CatalogEntry {
        id: String,
    }

    fn catalog_ids() -> Vec<String> {
        let entries: Vec<CatalogEntry> = serde_json::from_str(include_str!(
            "../../tests/fixtures/cursor-usable-models-20260818.json"
        ))
        .expect("fixture");
        entries.into_iter().map(|entry| entry.id).collect()
    }

    #[test]
    fn matches_every_composer_id_the_live_catalog_capture_serves() {
        let composer_ids: Vec<String> = catalog_ids().into_iter().filter(|id| id.to_lowercase().contains("composer")).collect();
        assert!(!composer_ids.is_empty());
        let unmatched: Vec<String> = composer_ids.into_iter().filter(|id| !is_cursor_composer_model(id)).collect();
        assert_eq!(unmatched, Vec::<String>::new());
    }

    #[test]
    fn leaves_every_non_composer_catalog_id_alone() {
        let matched: Vec<String> = catalog_ids()
            .into_iter()
            .filter(|id| !id.to_lowercase().contains("composer"))
            .filter(|id| is_cursor_composer_model(id))
            .collect();
        assert_eq!(matched, Vec::<String>::new());
    }

    #[test]
    fn keeps_cursors_resold_first_party_kimi_models_out_of_the_composer_family() {
        for id in ["kimi-k2.7-code", "kimi-k3-high", "kimi-k3-low"] {
            assert!(!is_cursor_composer_model(id), "{id} must not be treated as composer");
        }
    }

    #[test]
    fn matches_provider_qualified_and_future_composer_ids() {
        for id in ["cursor/composer-2.5", "composer-3", "composer-2.7-thinking-max"] {
            assert!(is_cursor_composer_model(id), "{id} must be treated as composer");
        }
    }
}
