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
pub mod banks;
pub mod extension;
pub mod legacy;
pub mod mcp;
pub mod recall;
mod schema;
pub mod store;
pub mod text;

pub use banks::{BankScope, Banks, Scoping};
pub use extension::{MemoryContext, MemoryExtension, TOOL_NAMES};
pub use legacy::{ImportOptions, ImportReport};
pub use mcp::ServerConfig;
pub use recall::{RecallOptions, RecallResult};
pub use store::{RememberOptions, Store};
