//! Port of omo-senpi `components/feedback` at TIP `7d818abaf9`.
//!
//! One upstream source file per Rust module:
//! - `collect.ts` -> [`collect`]
//! - `collect-host.ts` -> [`collect_host`]
//! - `draft.ts` -> [`draft`]
//! - `submit.ts` -> [`submit`]
//! - `runtime.ts` -> [`runtime`]
//! - `index.ts` (+ `runtime-loader.ts`) -> [`index`]

pub mod collect;
pub mod collect_host;
pub mod draft;
pub mod index;
pub mod runtime;
pub mod submit;

pub use draft::{
    FEEDBACK_SECTION_IDS, FeedbackDestination, FeedbackDraft, FeedbackDraftSection, FeedbackKind,
    FeedbackSectionId, REPORT_RETENTION_DAYS, Redaction, UploadRedactor,
};
pub use index::{
    FEEDBACK_COMPONENT_NAME, FEEDBACK_PREPARE_RPC_METHOD, FEEDBACK_SUBMIT_RPC_METHOD,
    FEEDBACK_UNAVAILABLE, FeedbackComponent, FeedbackComponentOptions, create_feedback_component,
    feedback_runtime_load_failure,
};
pub use submit::{
    FEEDBACK_IDEMPOTENCY_KEY_PATTERN, FEEDBACK_REDACTOR_VERSION, FeedbackSubmitRefusal,
    ReportTransport, SubmitResult,
};
