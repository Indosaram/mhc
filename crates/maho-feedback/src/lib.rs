//! Feedback and bug reports (docs/cleanroom/specs/feedback.md).
//!
//! A report is first prepared as a [`Draft`]: a fixed list of sections whose text is exactly what
//! will be uploaded, with secrets already removed by the host's upload redactor. Submitting posts
//! the draft to the report intake, or writes it to a private local file when uploads are off.
//!
//! - [`model`]: draft, sections, report kinds and submit results (wire shapes).
//! - [`collect`]: the host-collected sections (environment, config pass, logs, transcript).
//! - [`client`]: client-supplied identity values and extra sections (RPC only).
//! - [`reports`]: the draft store, prepare/submit and the RPC JSON entry points.
//! - [`flow`]: the `/feedback` / `/bug` command flow over a small dialog interface.
//! - [`extension`]: the `<builtin:feedback>` extension wiring it into a session.

pub mod client;
pub mod collect;
pub mod extension;
pub mod flow;
pub mod model;
pub mod reports;

pub use extension::{FeedbackExtension, PREPARE_RPC, SUBMIT_RPC, http_transport};
pub use model::{
    ClientInfo, Destination, Draft, PrepareOutcome, Refusal, ReportKind, Section, SectionId,
    SubmitResult,
};
pub use reports::{
    Host, HttpRequest, HttpResponse, LiveSession, PrepareRequest, Redacted, Reports, Store,
    SubmitRequest, shared_store,
};
