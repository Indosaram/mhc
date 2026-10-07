//! Kibitzer recall consumer core (latest `recall-wiring.ts` and `recall-drain.ts`).
//!
//! The ctx-free remainder of collection: everything here runs off plain captured values plus the
//! already-loaded corpus, so it is deterministic and testable. Hook registration and the resident
//! sidecar judge are separate (reported in the receipt).

use std::collections::BTreeSet;

use memory_core::recall::{
    RecallCandidate, RecallDocument, RecallLedger, RecallNudge, RecallSurfacedEntry,
    SelectRecallOptions, plan_recall_queries, render_nudge_block, select_recall_candidates,
};
use serde_json::Value;

use crate::recall_session_read::{RecallSessionSnapshot, RecallTranscriptTurn, judge_transcript, user_texts};

/// Provenance recorded next to a Kibitzer-surfaced path in the session ledger.
pub const GATE_SURFACE_HASH: &str = "kibitzer-gate";

/// Everything the Kibitzer sidecar needs about one hook's lexical candidates.
#[derive(Debug, Clone)]
pub struct CollectedRecallCandidates {
    pub session_id: String,
    pub identity: String,
    pub candidates: Vec<RecallCandidate>,
    /// Already-surfaced paths: the persona sees them, the parent validator re-checks them.
    pub surfaced: BTreeSet<String>,
    /// Authoritative cap (`memory.recall.max_items`) resolved for the bound agent.
    pub max_items: usize,
    /// The judge's window: USER + ASSISTANT, oldest first.
    pub transcript: Vec<RecallTranscriptTurn>,
}

/// Inputs for the ctx-free candidate collection.
pub struct CollectRecallCandidatesInput<'a> {
    /// Already-loaded corpus documents (the caller loads them through `RecallCorpusCache`).
    pub documents: &'a [RecallDocument],
    pub snapshot: &'a RecallSessionSnapshot,
    pub identity: &'a str,
    /// Resolved agent recall settings (`resolve_agent_recall_settings`).
    pub recall_settings: &'a Value,
    pub ledger: &'a RecallLedger,
    /// Tool-argument harvest, newest first.
    pub extra_texts: &'a [String],
    /// Paths already visible in the transcript (the transcript-mention exclusion seam).
    pub exclude_paths: &'a BTreeSet<String>,
}

/// Collects the lexical candidates for one completed turn, or `None` when there are none.
pub fn collect_recall_candidates(
    input: CollectRecallCandidatesInput<'_>,
) -> Result<Option<CollectedRecallCandidates>, String> {
    if input.recall_settings.get("enabled").and_then(Value::as_bool) == Some(false) {
        return Ok(None);
    }
    let max_items = input
        .recall_settings
        .get("max_items")
        .and_then(Value::as_u64)
        .unwrap_or(0) as usize;

    let texts = user_texts(&input.snapshot.entries);
    if texts.is_empty() && input.extra_texts.is_empty() {
        return Ok(None);
    }
    let queries = if input.extra_texts.is_empty() {
        plan_recall_queries(&texts, None)
    } else {
        let reversed: Vec<String> = input.extra_texts.iter().rev().cloned().collect();
        plan_recall_queries(&texts, Some(&reversed))
    };
    if queries.is_empty() || input.documents.is_empty() {
        return Ok(None);
    }

    let surfaced = input.ledger.surfaced_paths(&input.snapshot.id);
    let candidates = select_recall_candidates(
        input.documents,
        &queries,
        &SelectRecallOptions {
            max_items,
            surfaced: surfaced.clone(),
            exclude_paths: Some(input.exclude_paths.clone()),
            strategy: None,
            expansions: None,
        },
    );
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(CollectedRecallCandidates {
        session_id: input.snapshot.id.clone(),
        identity: input.identity.to_string(),
        candidates,
        surfaced,
        max_items,
        transcript: judge_transcript(&input.snapshot.entries),
    }))
}

/// Delivery of the nudges a turn should inject, already rendered.
#[derive(Debug, Clone)]
pub struct RecallInjection {
    pub message_content: String,
    pub paths: Vec<String>,
    pub nudges: Vec<RecallNudge>,
    pub session_id: String,
}

/// Union of the queued and pending-file nudges, deduped by path (queued wins).
pub fn recall_drain_union(queued: &[RecallNudge], from_file: &[RecallNudge]) -> Vec<RecallNudge> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    queued
        .iter()
        .chain(from_file.iter())
        .filter(|nudge| seen.insert(nudge.path.clone()))
        .cloned()
        .collect()
}

/// Builds the hidden recall message for the delivered nudges; `None` when there is nothing to say.
pub fn build_recall_injection(nudges: &[RecallNudge], session_id: &str) -> Option<RecallInjection> {
    if nudges.is_empty() {
        return None;
    }
    Some(RecallInjection {
        message_content: nudges
            .iter()
            .map(render_nudge_block)
            .collect::<Vec<_>>()
            .join("\n"),
        paths: nudges.iter().map(|nudge| nudge.path.clone()).collect(),
        nudges: nudges.to_vec(),
        session_id: session_id.to_string(),
    })
}

/// Marks the delivered paths surfaced; advisory, so a caller failure never suppresses a nudge.
pub fn mark_recall_surfaced(
    ledger: &RecallLedger,
    session_id: &str,
    nudges: &[RecallNudge],
) -> std::io::Result<()> {
    let entries: Vec<RecallSurfacedEntry> = nudges
        .iter()
        .map(|nudge| RecallSurfacedEntry {
            path: nudge.path.clone(),
            hash: GATE_SURFACE_HASH.to_string(),
        })
        .collect();
    ledger.mark_surfaced(session_id, &entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::TempDir;

    fn document(path: &str, description: &str, body: &str) -> RecallDocument {
        RecallDocument {
            path: path.to_string(),
            description: description.to_string(),
            body: body.to_string(),
        }
    }

    fn user_message(text: &str) -> Value {
        json!({ "type": "message", "message": { "role": "user", "content": text } })
    }

    fn nudge(path: &str, hint: &str) -> RecallNudge {
        RecallNudge {
            path: path.to_string(),
            hint: hint.to_string(),
        }
    }

    fn empty_excludes() -> BTreeSet<String> {
        BTreeSet::new()
    }

    #[test]
    fn given_recall_disabled_when_collecting_then_nothing_returns() {
        let dir = TempDir::new().unwrap();
        let ledger = RecallLedger::new(dir.path());
        let documents = [document("notes/a.md", "deploy", "rollback the canary")];
        let snapshot = crate::recall_session_read::snapshot_session("s1", &[user_message("canary")]).unwrap();
        let result = collect_recall_candidates(CollectRecallCandidatesInput {
            documents: &documents,
            snapshot: &snapshot,
            identity: "agent",
            recall_settings: &json!({ "enabled": false, "max_items": 2 }),
            ledger: &ledger,
            extra_texts: &[],
            exclude_paths: &empty_excludes(),
        })
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn given_a_matching_corpus_when_collecting_then_candidates_and_transcript_return() {
        let dir = TempDir::new().unwrap();
        let ledger = RecallLedger::new(dir.path());
        let documents = [
            document("notes/a.md", "deploy", "rollback the canary before the release"),
            document("notes/b.md", "unrelated", "nothing to see here"),
        ];
        let snapshot =
            crate::recall_session_read::snapshot_session("s1", &[user_message("canary")]).unwrap();
        let collected = collect_recall_candidates(CollectRecallCandidatesInput {
            documents: &documents,
            snapshot: &snapshot,
            identity: "agent",
            recall_settings: &json!({ "enabled": true, "max_items": 2 }),
            ledger: &ledger,
            extra_texts: &[],
            exclude_paths: &empty_excludes(),
        })
        .unwrap()
        .expect("candidates");

        assert_eq!(collected.session_id, "s1");
        assert_eq!(collected.identity, "agent");
        assert_eq!(collected.max_items, 2);
        assert_eq!(collected.candidates.len(), 1);
        assert_eq!(collected.candidates[0].path, "notes/a.md");
        assert_eq!(collected.transcript.len(), 1);
    }

    #[test]
    fn given_no_user_texts_and_no_extra_texts_when_collecting_then_nothing_returns() {
        let dir = TempDir::new().unwrap();
        let ledger = RecallLedger::new(dir.path());
        let documents = [document("notes/a.md", "deploy", "rollback the canary")];
        let snapshot = crate::recall_session_read::snapshot_session("s1", &[]).unwrap();
        let result = collect_recall_candidates(CollectRecallCandidatesInput {
            documents: &documents,
            snapshot: &snapshot,
            identity: "agent",
            recall_settings: &json!({ "enabled": true, "max_items": 2 }),
            ledger: &ledger,
            extra_texts: &[],
            exclude_paths: &empty_excludes(),
        })
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn given_a_surfaced_path_when_collecting_then_it_is_not_offered_again() {
        let dir = TempDir::new().unwrap();
        let ledger = RecallLedger::new(dir.path());
        ledger
            .mark_surfaced(
                "s1",
                &[RecallSurfacedEntry {
                    path: "notes/a.md".to_string(),
                    hash: "x".to_string(),
                }],
            )
            .unwrap();
        let documents = [document("notes/a.md", "deploy", "rollback the canary")];
        let snapshot =
            crate::recall_session_read::snapshot_session("s1", &[user_message("canary")]).unwrap();
        let result = collect_recall_candidates(CollectRecallCandidatesInput {
            documents: &documents,
            snapshot: &snapshot,
            identity: "agent",
            recall_settings: &json!({ "enabled": true, "max_items": 2 }),
            ledger: &ledger,
            extra_texts: &[],
            exclude_paths: &empty_excludes(),
        })
        .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn given_queued_and_pending_nudges_when_unioned_then_the_queued_copy_wins_and_order_is_kept() {
        let queued = vec![nudge("a.md", "queued"), nudge("b.md", "queued-b")];
        let from_file = vec![nudge("a.md", "file"), nudge("c.md", "file-c")];
        let union = recall_drain_union(&queued, &from_file);
        assert_eq!(
            union,
            vec![
                nudge("a.md", "queued"),
                nudge("b.md", "queued-b"),
                nudge("c.md", "file-c"),
            ]
        );
    }

    #[test]
    fn given_no_nudges_when_building_an_injection_then_none_returns() {
        assert!(build_recall_injection(&[], "s1").is_none());
    }

    #[test]
    fn given_nudges_when_building_an_injection_then_blocks_join_and_paths_are_listed() {
        let nudges = vec![
            nudge("a.md", "The note records the deploy gate."),
            nudge("b.md", "The note records the rollback order."),
        ];
        let injection = build_recall_injection(&nudges, "s1").expect("injection");
        assert_eq!(injection.session_id, "s1");
        assert_eq!(injection.paths, vec!["a.md".to_string(), "b.md".to_string()]);
        assert_eq!(injection.message_content.matches("<recalled-memory").count(), 2);
    }

    #[test]
    fn given_delivered_nudges_when_marking_surfaced_then_the_ledger_records_the_gate_hash() {
        let dir = TempDir::new().unwrap();
        let ledger = RecallLedger::new(dir.path());
        let nudges = vec![nudge("a.md", "The note records the deploy gate.")];
        mark_recall_surfaced(&ledger, "s1", &nudges).unwrap();
        assert_eq!(
            ledger.surfaced_paths("s1"),
            BTreeSet::from(["a.md".to_string()])
        );
        let raw = std::fs::read_to_string(dir.path().join("s1.json")).unwrap();
        let parsed: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed["surfaced"]["a.md"]["hash"], json!(GATE_SURFACE_HASH));
    }
}
