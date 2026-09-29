//! Reflection scheduling, isolated worktrees, state machine, and persona assets.

pub mod assets;
pub mod completion_validation;
pub mod machine;
pub mod reservation;
pub mod worktree;
pub mod worktree_integration;

pub use assets::{
    DREAM_PERSONA_MARKDOWN, REFLECTION_PERSONA_MARKDOWN, ReflectionPersona,
    ReflectionPersonaSection, load_dream_persona, load_reflection_persona, parse_sections,
};
pub use completion_validation::{CompletionValidation, validate_completion};
pub use machine::{
    CapturedConversation, CompleteTransition, DreamOrigin, EvaluationAction, EvaluationResult,
    JournalSnapshot, MachineState, ReflectionEvent, ReflectionOutcome, ReflectionRequest,
    ReflectionTrigger, ReservationState, ReservedRun, TriggerConfig, complete_transition,
    evaluate_transitions, reserve_transition,
};
pub use reservation::{
    CompletionResult, ReflectionLauncherIdentity, ReflectionReservationStore,
    ReflectionReservationStoreOptions, ReservationError, ReservationResult,
};
pub use worktree::{
    ReflectionCleanupReceipt, ReflectionFinalizeMode, ReflectionFinalizeResult, ReflectionWorktree,
    ReflectionWorktreeIdentity, WorktreeError, create_reflection_worktree,
    discard_reflection_worktree, finalize_reflection_worktree,
};
pub use worktree_integration::{
    IntegrateValidatedReflectionInput, LegacyAutoRunReceiptProbe, ReflectionIntegrationMode,
    ReflectionIntegrationProbe, ReflectionIntegrationResult, ValidatedReflectionTip,
    cleanup_reflection_worktree, integrate_validated_reflection, probe_legacy_auto_run_receipt,
    probe_reflection_integration,
};
