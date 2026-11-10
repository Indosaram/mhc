//! Long-term agent memory for mhc (MIT; see `PROVENANCE.md`).
//!
//! A Rust port of the MIT mnemopi engine from oh-my-pi: memories live in SQLite banks
//! ([`store::Store`], mnemopi's `working_memory`/`episodic_memory` schema with FTS5 mirrors), are
//! found by lexical hybrid recall ([`recall`]: FTS rank, keyword and word-form relevance,
//! synonyms, importance, recency, trust weights), and are reached three ways:
//!
//! - [`extension::MemoryExtension`]: the agent tools `retain`, `recall`, `memory_get`,
//!   `memory_forget`, a memory section in the system prompt, first-turn auto-recall, `/memory`;
//! - [`mcp::ServerConfig`]: a stdio MCP server with the `mnemopi_*` tools;
//! - [`legacy::import`]: a one-way importer for the legacy Markdown memory store, implemented from
//!   `docs/cleanroom/specs/memory-format.md`.
//!
//! Banks: the global bank is `<data>/mnemopi.db`, other banks `<data>/banks/<name>/mnemopi.db`;
//! sessions write to a per-project bank and also recall the global bank by default
//! ([`banks::Scoping`]). There are no embeddings and no LLM consolidation step in this port.
pub mod agreements;
pub mod banks;
pub mod extension;
pub mod facts_queue;
pub mod kibitzer;
pub mod legacy;
pub mod mcp;
pub mod people_store;
pub mod prompt;
pub mod recall;
pub mod records;
pub mod reflection;
mod schema;
pub mod store;
pub mod text;

pub use agreements::{Agreement, Agreements, Block, Projection, ProjectionError};
pub use banks::{BankScope, Banks, Scoping};
pub use extension::{MemoryContext, MemoryExtension, TOOL_NAMES};
pub use facts_queue::{Claims, Consumed, EnqueueRefusal, FactsBatch, Failures};
pub use kibitzer::{DeliveryVia, Kibitzer, Nudge, ToolResultGate};
pub use legacy::{ImportOptions, ImportReport};
pub use mcp::ServerConfig;
pub use people_store::{PeopleBook, WriteOutcome, WriteRefusal};
pub use prompt::{PromptBudget, SLOT_ORDER, assemble};
pub use recall::{RecallOptions, RecallResult};
pub use records::{Fact, FactFile, Observation, Observations, Person, PersonCard};
pub use reflection::{
    DreamOrigin, DreamPolicy, DreamState, ReflectionEvent, ReflectionOutcome, ReflectionTrigger,
    RunRecord,
};
pub use store::{RememberOptions, Store};
