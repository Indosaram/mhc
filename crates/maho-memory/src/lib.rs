//! Long-term agent memory for mhc (MIT; see `PROVENANCE.md`).
//!
//! A Rust port of the MIT mnemopi engine from oh-my-pi: memories live in SQLite banks
//! ([`store::Store`], mnemopi's `working_memory`/`episodic_memory` schema with FTS5 mirrors), are
//! found by lexical hybrid recall ([`recall`]: FTS rank, keyword and word-form relevance,
//! synonyms, importance, recency, trust weights), and are reached three ways:
//!
//! - [`extension::MemoryExtension`]: the agent tools `retain`, `recall`, `memory_get`,
//!   `memory_forget`, a memory section in the system prompt, first-turn auto-recall, `/memory`;
//! - [`mcp::ServerConfig`]: a stdio MCP server with the `mnemopi_*` tools;
//! - [`legacy::import`]: a one-way importer for the legacy Markdown memory store, implemented from
//!   `docs/cleanroom/specs/memory-format.md`.
//!
//! Banks: the global bank is `<data>/mnemopi.db`, other banks `<data>/banks/<name>/mnemopi.db`;
//! sessions write to a per-project bank and also recall the global bank by default
//! ([`banks::Scoping`]). There are no embeddings and no LLM consolidation step in this port.
pub mod agreements;
pub mod audit;
pub mod banks;
pub mod bm25;
pub mod broken_links;
pub mod completion_repair;
pub mod consent;
pub mod cursor;
pub mod description_preflight;
pub mod english_stem;
pub mod expiry;
pub mod extension;
pub mod fact_display;
pub mod facts_abort;
pub mod facts_anchor;
pub mod facts_batch_range;
pub mod facts_commit;
pub mod facts_extraction;
pub mod facts_failures_schema;
pub mod facts_note;
pub mod facts_parked;
pub mod facts_person_routing;
pub mod facts_queue;
pub mod facts_queue_scan;
pub mod facts_schema;
pub mod failure_backoff;
pub mod failure_selection;
pub mod frontmatter;
pub mod frontmatter_entries;
pub mod fs_resilience;
pub mod git_layout;
pub mod git_log;
pub mod git_status;
pub mod git_tree;
pub mod host_identity;
pub mod identity_layout;
pub mod journal;
pub mod kibitzer;
pub mod ledger;
pub mod legacy;
pub mod lock_acquire;
pub mod lock_record;
pub mod locks;
pub mod lookalike_keys;
pub mod mcp;
pub mod normalize;
pub mod orphan_sweep;
pub mod paths;
pub mod payload_cap;
pub mod people_store;
pub mod person_index;
pub mod persona_assets;
pub mod project_corpus;
pub mod prompt;
pub mod prompt_render;
pub mod random_ids;
pub mod recall;
pub mod recall_gate;
pub mod recall_planner;
pub mod recall_render;
pub mod recall_strategy;
pub mod recall_wire;
pub mod receipts;
pub mod records;
pub mod redact;
pub mod reflection;
pub mod reflection_completion;
pub mod reflection_machine;
pub mod reminders;
pub mod retention;
mod schema;
pub mod secret_scan_records;
pub mod seeds;
pub mod select;
pub mod session_provider;
pub mod soul_paths;
pub mod stale_holder;
pub mod store;
pub mod text;
pub mod transcript_search;

pub use agreements::{Agreement, Agreements, Block, Projection, ProjectionError};
pub use banks::{BankScope, Banks, Scoping};
pub use bm25::{
    QueryExpansions, RankedDocument, RecallDocument, expansion_weights, rank_documents,
    recall_document_terms, recall_terms, tokenize_recall_text,
};
pub use cursor::{
    CursorError, REFLECTION_SNAPSHOT_MAX_BYTES, REFLECTION_STATE_SCHEMA_VERSION,
    ReflectionSnapshot, ReflectionTranscriptState, TranscriptEntry, capture_cursor_snapshot,
    capture_cursor_snapshot_with_max_bytes, count_completed_steps, derive_state,
    derive_state_with_total, finalize_cursor, initial_reflection_state, journal_byte_offset,
    parse_state, reflected_through_byte_offset,
};
pub use description_preflight::{
    TrimmedDescription, description_length, description_spacing_end, is_oversized,
    replace_description_line, trim_description,
};
pub use extension::{MemoryContext, MemoryExtension, TOOL_NAMES};
pub use facts_queue::{Claims, Consumed, EnqueueRefusal, FactsBatch, Failures};
pub use failure_backoff::{
    ApplyFailureInput, BACKOFF_MS, DETAIL_MAX_BYTES, FailureFilter, FailureReason, FailureRecord,
    FailureState, FailureTarget, PARK_AT_STREAK, apply_failure, clear_for_retry, clear_on_success,
    delay_for, sanitize_detail, sort_records,
};
pub use failure_selection::{LaunchSelection, SkipReason, select_launchable, selection_key};
pub use frontmatter::{
    MAX_DESCRIPTION_LENGTH, decode_quoted_scalar, decode_scalar_source, decode_string_array,
    describe_description_violation, describe_frontmatter_grammar_violation,
    describe_frontmatter_violation, describe_header_grammar_violation, describe_header_violation,
    find_tool_call_scaffolding, is_canonical_scalar_source, is_plain_safe_scalar,
    parse_header_line, render_document, render_raw_scalar, render_string_scalar, split_frontmatter,
};
pub use git_log::{Commit, LOG_FORMAT, parse_log, parse_log_with_paths, parse_trailers};
pub use git_status::{Change, StatusEntry, StatusError, changed_paths, parse as parse_status};
pub use git_tree::{
    BlobEntry, SizedEntry, TreeError, parse_cat_file_batch, parse_ls_tree_blobs,
    parse_ls_tree_sized,
};
pub use journal::{Entry, Event, Journal, JournalError, Replayed};
pub use kibitzer::{DeliveryVia, Kibitzer, Nudge, ToolResultGate};
pub use ledger::{LedgerFile, SessionLedger, Surfaced, content_hash, sanitize_session_filename};
pub use legacy::{ImportOptions, ImportReport};
pub use lock_acquire::{
    AcquireError, Freshness, INCOMPLETE_LOCK_GRACE_MS, Liveness, LocalHost, LockContention,
    WithLockError, freshness, is_held, open_lock_handle, publish, read_owner, release, try_acquire,
    with_lock,
};
pub use lock_record::{
    CreateLockRecordOptions, LockHost, LockRecord, LockRecordError, create_lock_record,
    parse_lock_record,
};
pub use locks::{
    LockDomain, LockDomainError, facts_queue_lock_path, facts_runs_lock_path,
    memory_writer_lock_path, notice_lock_path, receipts_lock_path, reflection_scheduler_lock_path,
    run_finalization_lock_path, skills_usage_lock_path, transcript_state_lock_path,
};
pub use mcp::ServerConfig;
pub use normalize::{
    FRONTMATTER_NORMALIZATION_MARKER, FRONTMATTER_NORMALIZATION_SUBJECT, FrontmatterError,
    MemoryFrontmatter, NormalizeOutcome, ParsedMemoryFile, normalize_contents, parse_memory_file,
    render_memory_file, sanitize_frontmatter_value,
};
pub use orphan_sweep::{
    LEGACY_BRANCH_PREFIX, OrphanKind, REFLECTION_BRANCH_PREFIX, REFLECTION_ORPHAN_GRACE_MS,
    ReflectionLeftovers, ReflectionOrphanReceipt, ReflectionOrphanSelection, ReflectionOwner,
    RegisteredReflectionWorktree, basename, is_epoch_prefixed, is_inside, is_live,
    is_reflection_branch, parse_reflection_owner, parse_registered_worktrees,
    select_reflection_orphans,
};
pub use paths::{
    MemoryPathError, ValidateMemoryPathOptions, is_memory_content_path, validate_memory_path,
    validate_repository_path, validate_repository_path_with_field,
};
pub use payload_cap::{
    CappedBatch, CappedBatchInput, MAX_FACTS_PAYLOAD_BYTES, OversizedEntry, Payload,
    PayloadEnvelope, PrimaryHuman, measure_payload_bytes, select_capped_batch,
    select_capped_batch_with, serialize_payload,
};
pub use people_store::{PeopleBook, WriteOutcome, WriteRefusal};
pub use prompt::{PromptBudget, SLOT_ORDER, assemble};
pub use recall::{RecallOptions, RecallResult};
pub use recall_wire::{SessionRecall, SessionRecallHit, recall_for_session};
pub use receipts::{
    DETAIL_LIMIT, Receipt, ReceiptEvent, ReceiptKind, ReceiptLog, read_journal, receipts_path,
};
pub use records::{Fact, FactFile, Observation, Observations, Person, PersonCard};
pub use redact::{
    MASK, SecretClass, SecretMatch, contains_secret_like_material, redact_secret_like_material,
    scan_secret_like_material, secret_classes,
};
pub use reflection::{
    DreamOrigin, DreamPolicy, DreamState, ReflectionEvent, ReflectionOutcome, ReflectionTrigger,
    RunRecord,
};
pub use secret_scan_records::{
    BlobRecord, FileVersion, SCANNED_BLOB_MODES, SUBMODULE_MODE, index_blobs, introduced_versions,
    is_scanned_blob, is_submodule, is_zero_sha, parse_blob_records, parse_raw_diff, short_sha,
};
pub use select::{
    EXCERPT_CHARS, RRF_K, RecallCandidate, SelectOptions, build_excerpt, select_candidates,
};
pub use stale_holder::{
    LockReadError, ProcessLiveness, ReflectionLockProbe, ReflectionLockVerdict,
    SystemReflectionLockProbe, classify_reflection_lock_holder,
};
pub use store::{RememberOptions, Store};
