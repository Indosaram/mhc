//! Port of senpi packages/coding-agent/src/core/compaction/index.ts.

pub mod branch_summarization;
#[allow(clippy::module_inception)] // mirrors senpi's compaction/compaction.ts
pub mod compaction;
pub mod ideal_settings;
pub mod lifecycle;
pub mod settings;
pub mod stream_watchdog;
pub mod stuck_overflow;
pub mod utils;
pub mod warm_anchor;

pub use branch_summarization::{
    BRANCH_SUMMARY_PREAMBLE, BRANCH_SUMMARY_PROMPT, BranchPreparation, BranchSummaryDetails, BranchSummaryResult,
    CollectEntriesResult, collect_entries_for_branch_summary, create_branch_compaction_preparation,
    get_message_from_entry, prepare_branch_entries,
};
pub use compaction::{
    BASE64_RUN_RE, CompactionDetails, CompactionPreparation, CompactionResult, ContextUsageEstimate, CutPointResult,
    ESTIMATED_IMAGE_CHARS, MAX_SCALED_RESERVE_TOKENS, RESERVE_WINDOW_FRACTION, SOURCE_CONTEXT_UPDATE_SUMMARIZATION_PROMPT,
    SUMMARIZATION_PROMPT, UPDATE_SUMMARIZATION_INSTRUCTIONS, calculate_context_tokens, combine_usage,
    context_messages_for_compaction_entry, estimate_context_tokens, estimate_tokens, find_cut_point,
    find_turn_start_index, find_valid_cut_points, get_assistant_usage, get_last_assistant_usage,
    get_message_from_entry_for_compaction, get_summarization_failure, is_cut_point_message, is_turn_start_message,
    prepare_compaction, resolve_effective_reserve_tokens, resolve_reserve_tokens, resolve_threshold_context_tokens,
    should_compact, update_summarization_prompt, weighted_chars,
};
pub use ideal_settings::{IdealCompactionSettings, default_ideal_compaction_settings};
pub use lifecycle::{
    BeginCompactionOperation, CompactionAbortController, CompactionFinishStatus, CompactionLifecycleCoordinator,
    CompactionLifecycleState, CompactionModelRef, CompactionOperation, CompactionStage, FinishCompactionOperation,
    begin_compaction_operation, finish_compaction_operation, initial_compaction_lifecycle_state,
    promote_compaction_operation,
};
pub use settings::{CompactionSettings, default_compaction_settings};
pub use stream_watchdog::{
    DEFAULT_SUMMARIZATION_IDLE_TIMEOUT_MS, DEFAULT_SUMMARIZATION_MAX_DURATION_MS, SUMMARIZATION_MAX_DURATION_CAP_MS,
    SUMMARIZATION_MAX_DURATION_PER_TOKEN_MS, SUMMARIZATION_TOTAL_BUDGET_MS, StreamDurationBudgetError,
    StreamIdleTimeoutError, SummarizationDeadline, SummarizationTotalBudgetError, create_summarization_deadline,
    now_ms, summarization_max_duration_ms, summarization_total_budget_ms,
};
pub use stuck_overflow::is_turn_stuck_on_context_overflow;
pub use utils::{
    FileOperations, SUMMARIZATION_SYSTEM_PROMPT, TOOL_RESULT_MAX_CHARS, compute_file_lists, content_text_for_summary,
    create_file_ops, extract_file_ops_from_message, extract_patched_paths, format_file_operations,
    normalize_patch_text, serialize_conversation, strip_heredoc, truncate_for_summary,
};
pub use warm_anchor::{
    WarmAnchorSnapshot, create_warm_anchor_snapshot, is_warm_summary_anchor_valid, latest_compaction_entry_id,
};
