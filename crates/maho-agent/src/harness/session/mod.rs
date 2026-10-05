//! Port of senpi packages/agent/src/harness/session/.

// The module tree mirrors the senpi directory: session/session.ts, compaction/compaction.ts.
#![allow(clippy::module_inception)]

pub mod commit;
pub mod context;
pub mod fork;
pub mod fork_policy;
pub mod in_memory_storage_state;
pub mod jsonl;
pub mod memory;
pub mod mutation_line;
pub mod session;
pub mod testing;
pub mod types;
pub mod values;

pub use commit::{
    CommittedEntryWrite, CommittedListWrite, CommittedUsageWrite, CommittedValueWrite, CommittedWrite,
    CommitValidationState, PreparedCommit, commit_write, insert_entry, insert_usage, materialize_committed_entry,
    prepare_storage_commit, validate_committed_writes,
};
pub use context::{SessionContextBuildOptions, build_context_entries, build_session_context, session_entry_to_context_messages};
pub use fork::{ForkDestinationSnapshot, ForkSourceSnapshot, create_fork_snapshot};
pub use jsonl::{
    JSONL_FORMAT_VERSION, JSONL_STORAGE_VERSION, JsonlSessionCreateOptions, JsonlSessionListOptions,
    JsonlSessionMetadata, JsonlSessionRepo, JsonlSessionRepoOptions, JsonlStorage, JsonlStorageHeader,
    JsonlStorageOptions,
};
pub use fork_policy::{ForkCurrentStatePlan, project_fork_current_state_write, select_branch_fork};
pub use memory::{MemorySessionFacade, MemorySessionRepo, MemorySessionRepoOptions, MemoryStorage, MemoryStorageOptions};
pub use mutation_line::MutationLine;
pub use testing::{GatingStorage, InstrumentedStorage, StorageDecorator};
pub use session::{
    SessionBranchExistsError, SessionError, SessionErrorKind, SessionInvalidBranchError, SessionInvariantError,
    SessionPendingAssistantMessageError, SessionResult, SessionUnknownTargetError, StorageBackedSession,
    StorageBackedSessionOptions, session_branch_exists_error, session_invalid_branch_error,
    session_invariant_error, session_pending_assistant_message_error, session_unknown_target_error,
};
pub use types::*;
pub use values::*;
