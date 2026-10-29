//! Structural code search, rewrite and rule scan tools (MIT, clean room; see `PROVENANCE.md` and
//! `docs/cleanroom/specs/ast-grep.md`).
//!
//! The `mhc-ast-grep` sibling binary serves three tools (`search`, `rewrite`, `scan`) as
//! newline-delimited JSON-RPC on stdio ([`server`]). They drive the external `ast-grep` program,
//! found by [`resolve`], with bounded output and time ([`exec`]). [`extension`] declares the server
//! lazily on the MCP client of the agent.

pub mod error;
pub mod exec;
pub mod extension;
pub mod hints;
pub mod params;
pub mod paths;
pub mod resolve;
pub mod schema;
pub mod server;
pub mod tools;

pub use error::{ERROR_CODES, Phase, ToolError};
pub use exec::{AbortHandle, AbortSignal, abort_channel};
pub use extension::{AstGrepExtension, SERVER_NAME};
pub use resolve::Toolchain;
pub use server::{
    Server, ServerOptions, ToolReply, USAGE, handle_message, idle_timeout, run_cli, serve,
};
pub use tools::Context;
