//! Compaction policy primitives ported from senpi's builtin compaction extension.
pub mod policy;
pub mod speculation_lead;
pub mod idle;
pub mod idle_retry;
pub mod summarization_retry;
