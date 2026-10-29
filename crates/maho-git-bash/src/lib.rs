//! Git Bash command runner over stdio MCP (MIT, clean room; see `PROVENANCE.md` and
//! `docs/cleanroom/specs/git-bash-mcp.md`).
//!
//! On native Windows the `mhc-git-bash` sibling binary locates Git for Windows' `bash.exe`
//! ([`lookup`]) and serves the `run`, `which_bash` and `diagnose` tools ([`server`]) as
//! newline-delimited JSON-RPC on stdin/stdout ([`protocol`]). On every other platform it exits at
//! once without serving. [`extension`] declares the server on the MCP client of the agent.
//!
//! Everything that touches the machine (platform, environment, file probes, `where bash`, the
//! command runner) goes through the [`Host`] trait so the behaviour can be tested on any host.

pub mod extension;
pub mod host;
pub mod lookup;
pub mod protocol;
pub mod runner;
pub mod server;
pub mod settings;

pub use extension::{GitBashMcpExtension, SERVER_NAME};
pub use host::{Host, RunOutput, RunRequest, SystemHost, node_platform};
pub use lookup::{Resolution, Source, resolve};
pub use protocol::{CliCommand, USAGE, parse_args, run_cli};
pub use server::{Server, ServerOptions, ToolReply};
