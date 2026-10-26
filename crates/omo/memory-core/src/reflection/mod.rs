//! Reflection scheduling, isolated worktrees, state machine, and persona assets.

pub mod assets;
pub mod completion_repair;
pub mod completion_secret_scan;
pub mod completion_validation;
pub mod description_preflight;
pub mod machine;
pub mod orphan_sweep;
pub mod reservation;
pub mod stale_holder;
pub mod worktree;
pub mod worktree_integration;

pub use assets::{
    DREAM_PERSONA_MARKDOWN, REFLECTION_PERSONA_MARKDOWN, ReflectionPersona,
    ReflectionPersonaSection, load_dream_persona, load_reflection_persona, parse_sections,
};
pub use completion_repair::{
    DROPPED_TRAILER, DroppedEntry, STABLE_LOG_OUTPUT, commit_count, describe_dropped_entries,
    read_dropped_trailers, squash_without_paths,
};
pub use completion_secret_scan::{
    SecretLikeFailure, find_secret_like_failure, find_secret_like_failures,
};
pub use completion_validation::{
    CompletionValidation, frontmatter_block, list_reflection_changed_paths, validate_completion,
};
pub use description_preflight::{
    DescriptionPreflightWarn, TrimmedDescription, preflight_reflection_descriptions,
};
pub use machine::{
    CapturedConversation, CompleteTransition, DreamOrigin, EvaluationAction, EvaluationResult,
    JournalSnapshot, MachineState, ReflectionEvent, ReflectionOutcome, ReflectionRequest,
    ReflectionTrigger, ReservationState, ReservedRun, TriggerConfig, complete_transition,
    evaluate_transitions, reserve_transition,
};
pub use orphan_sweep::{
    OrphanKind, REFLECTION_ORPHAN_GRACE_MS, ReflectionLeftovers, ReflectionOrphanReceipt,
    ReflectionOrphanSelection, ReflectionOrphanSweepOptions, RegisteredReflectionWorktree,
    list_reflection_leftovers, select_reflection_orphans, sweep_reflection_orphans,
};
pub use reservation::{
    CompletionResult, ReflectionLauncherIdentity, ReflectionReservationStore,
    ReflectionReservationStoreOptions, ReservationError, ReservationResult,
};
pub use stale_holder::{
    LockReadError, ReflectionLockProbe, ReflectionLockVerdict, SystemReflectionLockProbe,
    classify_reflection_lock_holder,
};
pub use worktree::{
    ReflectionCleanupReceipt, ReflectionFinalizeMode, ReflectionFinalizeResult, ReflectionWorktree,
    ReflectionWorktreeIdentity, WorktreeError, create_reflection_worktree,
    discard_reflection_worktree, finalize_reflection_worktree,
    finalize_reflection_worktree_with_warn,
};
pub use worktree_integration::{
    IntegrateValidatedReflectionInput, LegacyAutoRunReceiptProbe, ReflectionIntegrationMode,
    ReflectionIntegrationProbe, ReflectionIntegrationResult, ValidatedReflectionTip,
    cleanup_reflection_worktree, integrate_validated_reflection, probe_legacy_auto_run_receipt,
    probe_reflection_integration,
};

pub mod stale_holder_proof;
