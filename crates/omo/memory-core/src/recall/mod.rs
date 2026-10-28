//! Recall subsystem (latest `packages/memory-core/src/recall`).

pub mod assets;
pub mod bm25;
pub mod bm25_persist;
pub mod bm25_persist_gate;
pub mod bm25_store;
pub mod bm25_terms_transfer;
mod corpus_derived;
pub mod english_stem;
pub mod gate;
pub mod haystack;
pub mod ledger;
pub mod planner;
pub mod project_corpus;
pub mod provider;
pub mod render;
pub mod select;
pub mod strategy;

pub use assets::{KIBITZER_PERSONA_MARKDOWN, kibitzer_persona, load_kibitzer_persona};
pub use bm25::{
    DocumentTerms, DocumentTermsInput, RECALL_TERM_ACCESS, RankedRecallDocument,
    RecallQueryExpansions, RecallTermAccess, RecallTermFigures, RecallTermsSnapshot,
    StoredDocumentTerms, is_cjk_char, is_han_character, on_recall_documents_encoded,
    rank_recall_documents_bm25, recall_document_terms, recall_expansion_weights, recall_term_figures,
    recall_terms, tokenize_recall_text,
};
pub use bm25_persist::{
    RECALL_TERM_STORE_NAME, RecallTermErrorHandler, RecallTermPersistence,
    with_recall_term_saves_excluded,
};
pub use bm25_persist_gate::{is_recall_term_save_excluded, track_recall_term_save};
pub use bm25_store::{RecallTermStore, decode_recall_term_store, encode_recall_term_store};
pub use bm25_terms_transfer::{seed_recall_document_terms, snapshot_recall_document_terms};
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
pub use project_corpus::{
    PROJECT_MEMORY_PATH_PREFIX, ProjectMemoryPath, parse_project_memory_path,
    qualify_project_documents,
};
pub use provider::{
    RecallCorpus, RecallCorpusCache, RecallCorpusCacheOptions, RecallDocument, RecallMemoryFigures,
    RecallTermStoreLoader, is_recall_candidate_path, load_recall_corpus, recall_memory_figures,
};
pub use render::{RECALL_HINT_HEADER, RECALL_HINT_HEADER_KO, render_nudge_block};
pub use select::{RecallCandidate, SelectRecallOptions, select_recall_candidates};
pub use strategy::{
    CJK_CORPUS_MIN_SHARE, LARGE_CORPUS_MIN_DOCUMENTS, RecallStrategy, choose_recall_strategy,
    corpus_cjk_share, has_cjk,
};
