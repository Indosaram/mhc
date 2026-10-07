//! Recall subsystem (latest `packages/memory-core/src/recall`).

pub mod assets;
pub mod bm25;
pub mod english_stem;
pub mod gate;
pub mod haystack;
pub mod ledger;
pub mod planner;
pub mod provider;
pub mod render;
pub mod select;
pub mod strategy;

pub use assets::{KIBITZER_PERSONA_MARKDOWN, kibitzer_persona, load_kibitzer_persona};
pub use bm25::{
    RankedRecallDocument, RecallQueryExpansions, is_cjk_char, is_han_character,
    rank_recall_documents_bm25, recall_expansion_weights, recall_terms, tokenize_recall_text,
};
pub use english_stem::stem_english_token;
pub use gate::{
    InvalidHintReason, NUDGE_HINT_MAX_CHARS, PENDING_NUDGES_VERSION, PendingNudges,
    PendingNudgesFile, RecallNudge, ValidateNudgesOptions, describe_invalid_hint, is_valid_hint,
    parse_pending_file, validate_nudges,
};
pub use haystack::normalized_haystack;
pub use ledger::{
    RECALL_LEDGER_VERSION, RecallLedger, RecallLedgerEntry, RecallLedgerFile, RecallSurfacedEntry,
    recall_ledger_disk_reads, sanitize_session_filename,
};
pub use planner::{MAX_RECALL_QUERIES, plan_recall_queries};
pub use provider::{
    RecallCorpus, RecallCorpusCache, RecallCorpusCacheOptions, RecallDocument,
    is_recall_candidate_path, load_recall_corpus,
};
pub use render::{RECALL_HINT_HEADER, RECALL_HINT_HEADER_KO, render_nudge_block};
pub use select::{RecallCandidate, SelectRecallOptions, select_recall_candidates};
pub use strategy::{
    CJK_CORPUS_MIN_SHARE, LARGE_CORPUS_MIN_DOCUMENTS, RecallStrategy, choose_recall_strategy,
    corpus_cjk_share, has_cjk,
};
