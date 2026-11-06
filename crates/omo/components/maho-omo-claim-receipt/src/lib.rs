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

pub use cell_commands::{CellCheckKind, CellInvokedKinds, cell_invoked_kinds};
pub use classify::{
    Claim, ClaimKind, ClaimStrength, classify_claims, join_soft_newlines, mask_quoted_prose,
    split_sentences,
};
pub use evidence::{
    CheckEvidence, CheckKind, CheckOutcome, EditEvidence, Timeline, build_timeline, content_text,
};
pub use index::{CLAIM_RECEIPT_NUDGE_FLAG, ClaimReceiptComponent};
pub use invocations::{InvokedKind, MAX_DEPTH, invoked_kinds};
pub use r#match::{
    ClaimStatus, ClaimVerdict, InvalidatedBy, Receipt, UnsupportedReason, is_flagged, match_claims,
};
pub use runtime::{AnswerVerdicts, evaluate_answer};
pub use shell_words::{shell_segments, strip_heredoc_bodies, without_comment};
pub use signatures::{is_relevant_path, judge_output, judge_structured};
pub use verdict::{
    GATE_FLAGS_TAG, NUDGE_MESSAGE_TYPE, VERDICT_ENTRY_TYPE, VerdictRecord, gate_flags_block,
    nudge_text, verdict_value,
};
