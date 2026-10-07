//! The sidecar's `memory` tool (latest `kibitzer/tools/memory.ts`).
//!
//! Read-only access to the COMMITTED memory corpus: `search` ranks committed documents with the
//! pinned lexical recall selector and `read` returns one committed body. Every byte comes from HEAD
//! through the recall corpus, never the working tree, and there is no write operation by design.
//!
//! Only the pure executor is ported here. The upstream factory's host-bound half - the
//! `budgeted(...)` wrapper that charges the per-wake tool budget and the registered `ToolDefinition`
//! (name, description and the two parameter schemas) - belongs to the member-tool registry
//! (`kibitzer_member_tools`), exactly as the CLI host owns `read`/`grep`/`session_entries`.
//! `execute_memory` therefore charges nothing: the registered tool charges the call ONCE on entry,
//! and charging here too would double-count one call against the wake.

use std::collections::BTreeSet;

use memory_core::git::{GitError, GitMemoryRepo};
use memory_core::recall::{
    RecallCorpus, RecallCorpusCache, RecallQueryExpansions, SelectRecallOptions,
    select_recall_candidates,
};
use serde_json::{Value, json};

use crate::kibitzer_tools_caps::KibitzerToolCaps;
use crate::kibitzer_tools_path_safety::{PathCheck, normalize_memory_path};
use crate::kibitzer_tools_result::{
    KibitzerRejectionCode, KibitzerToolResult, bounded_text, ok_json, ok_text, rejection,
};

/// The registered tool name (`memory`).
pub const KIBITZER_MEMORY_TOOL_NAME: &str = "memory";

/// The ONLY operations the sidecar's memory tool has. There is no write operation, by design.
pub const MEMORY_TOOL_OPERATIONS: [&str; 2] = ["search", "read"];

/// Bounds of the terms a search may add; the same as birkin-mnemosyne's `memory_search`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryExpansionBounds {
    pub terms: usize,
    pub term_chars: usize,
    pub note_line_chars: usize,
}

/// The one shared set of bounds (upstream `MEMORY_EXPANSION_BOUNDS`).
pub const MEMORY_EXPANSION_BOUNDS: MemoryExpansionBounds = MemoryExpansionBounds {
    terms: 16,
    term_chars: 80,
    note_line_chars: 300,
};

/// Runs one `memory` call against the committed corpus.
///
/// `cache` is the member-owned recall cache and `searched_paths` the member-owned set the nudge tool
/// later accepts from; both live for the sidecar's lifetime and neither is a global. `query_expansion`
/// is `memory.recall.query_expansion`: off, the search keeps the two fields it always had and ignores
/// added terms; on, the added terms are validated and may widen the search.
///
/// The corpus is loaded through the cache on EVERY search and read, so a HEAD move refreshes it. A
/// load failure is returned as `Err` - upstream awaits `corpus()` and lets the rejection propagate -
/// and it precedes any argument rejection, matching upstream's left-to-right argument evaluation.
pub fn execute_memory(
    repo: &GitMemoryRepo,
    cache: &mut RecallCorpusCache,
    caps: KibitzerToolCaps,
    query_expansion: bool,
    searched_paths: &mut BTreeSet<String>,
    params: &Value,
) -> Result<KibitzerToolResult, GitError> {
    match params.get("operation").and_then(Value::as_str) {
        Some("search") => {
            let corpus = cache.load(repo)?;
            Ok(search(
                params.get("query").and_then(Value::as_str),
                query_expansion.then_some(params),
                &corpus,
                caps,
                searched_paths,
            ))
        }
        Some("read") => {
            let corpus = cache.load(repo)?;
            Ok(read(params.get("path").and_then(Value::as_str), &corpus, caps))
        }
        _ => Ok(rejection(
            KibitzerRejectionCode::UnsupportedOperation,
            &format!("Unsupported operation; use one of: {}.", MEMORY_TOOL_OPERATIONS.join(", ")),
            None,
        )),
    }
}

/// One search: at most `caps.memory_search_results` committed hits at the current HEAD revision, and
/// the exact paths returned are admitted to `searched_paths`.
fn search(
    query: Option<&str>,
    added: Option<&Value>,
    corpus: &RecallCorpus,
    caps: KibitzerToolCaps,
    searched_paths: &mut BTreeSet<String>,
) -> KibitzerToolResult {
    let Some(query) = query.filter(|query| !query.trim().is_empty()) else {
        return rejection(KibitzerRejectionCode::MissingArgument, "search requires a non-empty query.", None);
    };
    let (expansions, expansion_fallback) = match added.map(read_expansions) {
        None => (None, None),
        Some(Ok(expansions)) => (Some(expansions), None),
        Some(Err(message)) => (None, Some(message)),
    };
    // Invalid added terms never lose plain lexical recall: the fallback is reported in the SUCCESSFUL
    // result so the sidecar can correct its next search without turning this search into a miss.
    // One over the cap tells us whether the page is truncated without a second selection pass.
    let selected = select_recall_candidates(
        &corpus.documents,
        &[query.to_string()],
        &SelectRecallOptions {
            max_items: caps.memory_search_results.saturating_add(1),
            // Upstream passes a FRESH empty surfaced set here, not the session's, so a search never
            // excludes a path merely for having been surfaced earlier.
            surfaced: BTreeSet::new(),
            exclude_paths: None,
            strategy: None,
            expansions,
        },
    );
    let truncated = selected.len() > caps.memory_search_results;
    let mut results: Vec<Value> = Vec::new();
    for candidate in selected.iter().take(caps.memory_search_results) {
        results.push(json!({
            "path": candidate.path,
            "description": bounded_text(&candidate.description, caps.memory_read_chars),
            "excerpt": bounded_text(&candidate.excerpt, caps.memory_read_chars),
        }));
        // Only the paths actually returned are admitted, so the nudge tool accepts exactly what the
        // sidecar could have read.
        searched_paths.insert(candidate.path.clone());
    }
    let mut body = json!({ "revision": corpus.revision, "results": results, "truncated": truncated });
    if let Some(fallback) = expansion_fallback {
        body["expansion_fallback"] = Value::String(fallback);
    }
    ok_json(&body)
}

/// One read: the committed `description` and body of `path`, redacted then bounded to
/// `caps.memory_read_chars`.
fn read(path: Option<&str>, corpus: &RecallCorpus, caps: KibitzerToolCaps) -> KibitzerToolResult {
    let Some(path) = path else {
        return rejection(KibitzerRejectionCode::MissingArgument, "read requires a path.", None);
    };
    let normalized = match normalize_memory_path(path) {
        PathCheck::Ok { path } => path,
        // A normalization failure carries the path the model SUPPLIED, not the rejected result.
        PathCheck::Rejected { code, message } => return rejection(code, &message, Some(path)),
    };
    let Some(document) = corpus.documents.iter().find(|document| document.path == normalized) else {
        return rejection(
            KibitzerRejectionCode::NotCommitted,
            &format!("\"{normalized}\" is not a committed memory file at HEAD."),
            Some(&normalized),
        );
    };
    ok_text(bounded_text(
        &format!("description: {}\n\n{}", document.description, document.body),
        caps.memory_read_chars,
    ))
}

/// The added terms of one search, or the reason they are refused.
///
/// The model wrote them, so the bounds are checked here: each tier must be an array of at most
/// `MEMORY_EXPANSION_BOUNDS.terms` non-empty strings of at most `MEMORY_EXPANSION_BOUNDS.term_chars`
/// Unicode scalar values, and `note_line` at most `MEMORY_EXPANSION_BOUNDS.note_line_chars`. The
/// FIRST offending tier in `synonyms`, `keywords`, `related` order wins, and an invalid set is a
/// fallback string, never a rejection.
fn read_expansions(params: &Value) -> Result<RecallQueryExpansions, String> {
    let mut tiers: [Option<Vec<String>>; 3] = [None, None, None];
    for (index, tier) in ["synonyms", "keywords", "related"].into_iter().enumerate() {
        let Some(terms) = params.get(tier) else {
            continue;
        };
        let Some(array) = terms.as_array() else {
            return Err(format!("{tier} is a list of terms."));
        };
        if array.len() > MEMORY_EXPANSION_BOUNDS.terms {
            return Err(format!("{tier} takes at most {} terms.", MEMORY_EXPANSION_BOUNDS.terms));
        }
        let mut values: Vec<String> = Vec::with_capacity(array.len());
        for term in array {
            let valid = term
                .as_str()
                .filter(|text| !text.is_empty() && text.chars().count() <= MEMORY_EXPANSION_BOUNDS.term_chars);
            let Some(text) = valid else {
                return Err(format!(
                    "every {tier} term is text of 1-{} characters.",
                    MEMORY_EXPANSION_BOUNDS.term_chars
                ));
            };
            values.push(text.to_string());
        }
        tiers[index] = Some(values);
    }
    let note_line = match params.get("note_line") {
        None => None,
        Some(Value::String(text)) if text.chars().count() <= MEMORY_EXPANSION_BOUNDS.note_line_chars => {
            Some(text.clone())
        }
        Some(_) => {
            return Err(format!(
                "note_line is text of at most {} characters.",
                MEMORY_EXPANSION_BOUNDS.note_line_chars
            ));
        }
    };
    let [synonyms, keywords, related] = tiers;
    Ok(RecallQueryExpansions { synonyms, keywords, related, note_line })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kibitzer_tools_caps::DEFAULT_KIBITZER_TOOL_CAPS;
    use memory_core::git::GitCommitAuthor;
    use memory_core::recall::RecallCorpusCacheOptions;
    use serde_json::{json, Value};

    fn temp_root() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    fn author() -> GitCommitAuthor {
        GitCommitAuthor {
            agent_id: "memory-agent".to_string(),
            author_name: "Memory Fixture".to_string(),
            author_email: None,
        }
    }

    fn memory_repo(root: &std::path::Path) -> GitMemoryRepo {
        let repo = GitMemoryRepo::open(root, "memory-agent").expect("repo");
        repo.init(None).expect("init");
        repo
    }

    fn commit_memory(repo: &GitMemoryRepo, path: &str, description: &str, body: &str) {
        let full = repo.dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&full, format!("---\ndescription: {description}\n---\n{body}\n")).expect("write");
        repo.commit_write(&[path], "save memory", &author()).expect("commit");
    }

    fn call(repo: &GitMemoryRepo, caps: KibitzerToolCaps, params: &Value) -> (KibitzerToolResult, BTreeSet<String>) {
        let mut cache = RecallCorpusCache::new(RecallCorpusCacheOptions::default());
        let mut searched = BTreeSet::new();
        let result = execute_memory(repo, &mut cache, caps, false, &mut searched, params).expect("execute_memory returns Ok");
        (result, searched)
    }

    fn json_of(result: &KibitzerToolResult) -> Value {
        serde_json::from_str(&result.text).expect("result text is JSON")
    }

    #[test]
    fn given_a_committed_memory_with_a_dirty_working_tree_when_read_then_the_head_body_is_returned() {
        let root = temp_root();
        let repo = memory_repo(root.path());
        commit_memory(&repo, "notes/creds.md", "Creds", "committed body from HEAD");
        // A working-tree edit that was never committed must never leak into a read.
        std::fs::write(repo.dir.join("notes/creds.md"), "---\ndescription: Creds\n---\nworking tree edit that is NOT committed\n").expect("dirty write");

        let (result, _searched) = call(&repo, DEFAULT_KIBITZER_TOOL_CAPS, &json!({ "operation": "read", "path": "notes/creds.md" }));

        assert!(!result.is_error, "a committed read is not an error");
        assert!(result.text.contains("committed body from HEAD"), "the HEAD body is returned");
        assert!(!result.text.contains("working tree edit"), "the dirty working tree must not leak into a read");
    }

    #[test]
    fn given_a_path_that_is_not_committed_when_read_then_it_is_rejected_as_not_committed() {
        let root = temp_root();
        let repo = memory_repo(root.path());
        commit_memory(&repo, "notes/present.md", "Present", "present body");

        let (result, _searched) = call(&repo, DEFAULT_KIBITZER_TOOL_CAPS, &json!({ "operation": "read", "path": "notes/absent.md" }));

        assert!(result.is_error, "a missing HEAD path is a rejection");
        let body = json_of(&result);
        assert_eq!(body["rejected"], json!("not_committed"));
        assert_eq!(body["path"], json!("notes/absent.md"));
    }

    #[test]
    fn given_more_matching_memories_than_the_cap_when_search_runs_then_the_page_is_bounded_and_only_returned_paths_are_recorded() {
        let root = temp_root();
        let repo = memory_repo(root.path());
        for index in 0..5 {
            commit_memory(&repo, &format!("notes/n{index}.md"), "Shared note", &format!("shared topic {index}"));
        }
        let caps = KibitzerToolCaps { memory_search_results: 2, ..DEFAULT_KIBITZER_TOOL_CAPS };

        let (result, searched) = call(&repo, caps, &json!({ "operation": "search", "query": "shared" }));

        assert!(!result.is_error, "a matching search is not an error");
        let body = json_of(&result);
        assert!(body["revision"].is_string(), "the search reports the corpus revision");
        let results = body["results"].as_array().expect("results is an array");
        assert_eq!(results.len(), 2, "the page is bounded by memory_search_results");
        assert_eq!(body["truncated"], json!(true), "more matches than the cap reports truncated");

        let returned: BTreeSet<String> = results.iter().map(|hit| hit["path"].as_str().expect("hit path").to_string()).collect();
        assert_eq!(searched, returned, "only the returned paths are recorded for nudging");
    }

    #[test]
    fn given_an_unknown_operation_when_called_then_the_rejection_names_unsupported_operation() {
        let root = temp_root();
        let repo = memory_repo(root.path());

        let (result, searched) = call(&repo, DEFAULT_KIBITZER_TOOL_CAPS, &json!({ "operation": "create", "path": "notes/new.md" }));

        assert!(result.is_error, "an unknown operation is a rejection");
        assert_eq!(json_of(&result)["rejected"], json!("unsupported_operation"));
        assert!(searched.is_empty(), "a rejected call records no searched paths");
    }
}
