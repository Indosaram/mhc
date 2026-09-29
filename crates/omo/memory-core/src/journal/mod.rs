//! Per-conversation transcript cursors, reflection snapshots, and durable journal state.

pub mod cursor;
pub mod entries;
pub mod lock;
pub mod store;

pub use cursor::{
    REFLECTION_STATE_SCHEMA_VERSION, ReflectionSnapshot, ReflectionTranscriptState,
    capture_cursor_snapshot, count_completed_steps, derive_state, finalize_cursor,
    initial_reflection_state, is_canonical_entry, parse_state,
};
pub use entries::{
    ProjectedReasoning, ProjectedToolCall, REDACTED_REASONING_TEXT, TOOL_ARGS_TRUNCATE_LIMIT,
    TextTranscriptEntry, ToolCallTranscriptEntry, TranscriptEntry, TranscriptProjection,
    parse_transcript_entry, project_transcript_entries,
};
pub use lock::{
    ACQUISITION_WAIT_MS, DefaultJournalLock, JournalLock, JournalLockTimeoutError, ProcessLiveness,
    RETRY_DELAY_MS, get_pid_liveness, get_process_start_identity, try_reclaim_stale_lock,
    with_local_journal_lock,
};
pub use store::{
    AppendResult, JournalError, TranscriptJournal, TranscriptJournalOptions,
    sync_journal_directory, sync_journal_file,
};
