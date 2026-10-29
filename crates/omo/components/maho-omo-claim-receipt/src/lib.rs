//! Native Rust port of upstream `packages/omo-senpi/src/components/claim-receipt`.
//!
//! One upstream file per module: `cell-commands.ts`, `classify.ts`, `evidence.ts`, `index.ts`,
//! `invocations.ts`, `match.ts`, `runtime.ts`, `shell-words.ts`, `signatures.ts`, `verdict.ts`.
//! `runtime-loader.ts` is JS-only sidecar bundling and has no native counterpart.

pub mod cell_commands;
pub mod classify;
pub mod evidence;
pub mod index;
pub mod invocations;
pub mod r#match;
pub mod runtime;
pub mod shell_words;
pub mod signatures;
pub mod verdict;

pub use cell_commands::{cell_invoked_kinds, CellCheckKind, CellInvokedKinds};
pub use classify::{classify_claims, mask_quoted_prose, split_sentences, Claim, ClaimKind, ClaimStrength};
pub use evidence::{
    build_timeline, content_text, CheckEvidence, CheckKind, CheckOutcome, EditEvidence, Timeline,
};
pub use index::{ClaimReceiptComponent, CLAIM_RECEIPT_NUDGE_FLAG};
pub use invocations::{invoked_kinds, InvokedKind, MAX_DEPTH};
pub use r#match::{
    is_flagged, match_claims, ClaimStatus, ClaimVerdict, InvalidatedBy, Receipt, UnsupportedReason,
};
pub use runtime::{evaluate_answer, AnswerVerdicts};
pub use shell_words::{shell_segments, strip_heredoc_bodies, without_comment};
pub use signatures::{is_relevant_path, judge_output, judge_structured};
pub use verdict::{
    gate_flags_block, nudge_text, verdict_value, VerdictRecord, GATE_FLAGS_TAG, NUDGE_MESSAGE_TYPE,
    VERDICT_ENTRY_TYPE,
};
