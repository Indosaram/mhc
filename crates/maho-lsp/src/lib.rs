//! Language Server Protocol support for mhc.
//!
//! - [`rpc`]: Content-Length framed JSON-RPC connection over any async byte stream.
//! - [`client`]: one language server (local child process or a daemon link): initialize, document
//!   sync, definition/references/symbols/diagnostics/rename requests.
//! - [`manager`]: shared pool keyed by workspace root + server id, with ref counts, init
//!   timeouts, idle reaping and dead-connection retry.
//! - [`config`] / [`servers`]: builtin server table merged with `lsp-client.json` overrides.
//! - [`tools`]: the six LSP tool operations rendered as text + JSON details.
//! - [`hooks`]: formatter and post-mutation hook interfaces; [`post_edit`] is the LSP one.
//! - [`daemon`] (unix): the `mhc-lsp` mux daemon that keeps language servers alive across
//!   mhc processes, and the client side that links to it.
//!
//! Ported from MIT `pi-lsp-client` and the MIT oh-my-pi LSP mux design (see PROVENANCE.md).

pub mod client;
pub mod config;
#[cfg(unix)]
pub mod daemon;
pub mod edit;
pub mod error;
#[cfg(feature = "extension")]
pub mod extension;
pub mod format;
pub mod hooks;
pub mod manager;
pub mod post_edit;
pub mod rpc;
pub mod servers;
pub mod tools;
pub mod types;

pub use client::{ClientTimings, LspClient};
pub use config::{ConfigPaths, ResolvedServer, ServerLookup};
pub use error::LspError;
pub use manager::{DaemonMode, LspManager, LspManagerOptions};

/// Default caps and timeouts.
pub mod limits {
    use std::time::Duration;
    pub const MAX_REFERENCES: usize = 200;
    pub const MAX_SYMBOLS: usize = 200;
    pub const MAX_DIAGNOSTICS: usize = 200;
    pub const MAX_DIRECTORY_FILES: usize = 50;
    pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
    pub const INIT_TIMEOUT: Duration = Duration::from_secs(60);
    pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
    pub const REAPER_INTERVAL: Duration = Duration::from_secs(60);
    pub const STOP_HARD_KILL_TIMEOUT: Duration = Duration::from_secs(5);
}
