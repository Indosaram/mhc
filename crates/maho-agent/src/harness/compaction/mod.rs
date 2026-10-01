//! Port of senpi packages/agent/src/harness/compaction/.

// The module tree mirrors the senpi directory: compaction/compaction.ts.
#![allow(clippy::module_inception)]

pub mod branch_summarization;
pub mod compaction;
pub mod utils;

pub use branch_summarization::{
    BranchPreparation, BranchSummaryDetails, BranchSummaryError, BranchSummaryErrorCode, BranchSummaryResult,
    CollectEntriesResult, GenerateBranchSummaryOptions, collect_entries_for_branch_summary,
    generate_branch_summary, generate_branch_summary_with_request, prepare_branch_entries,
};
pub use compaction::{
    CompactGenerationOptions, CompactResult, CompactionDetails, CompactionError, CompactionErrorCode,
    CompactionPreparation, CompactionSettings, ContextUsageEstimate, CutPointResult, DEFAULT_COMPACTION_SETTINGS,
    SummaryGenerationOptions, SummaryRequest, calculate_context_tokens, compact, compact_with_request,
    complete_simple_with_retries, create_summary_request_options, estimate_context_tokens, estimate_tokens,
    find_cut_point, find_turn_start_index, generate_summary, generate_summary_with_request,
    generate_summary_with_usage, get_last_assistant_usage, prepare_compaction, serialize_conversation,
    should_compact, SUMMARIZATION_SYSTEM_PROMPT,
};
pub use utils::{
    FileOperations, compute_file_lists, content_text_for_summary, create_file_ops, extract_file_ops_from_message,
    format_file_operations,
};
