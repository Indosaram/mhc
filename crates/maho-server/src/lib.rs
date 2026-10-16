//! Shared-host and app-server layer for `mhc`: the daemon RPC protocol frames (`protocol`), its
//! client and server, the Codex app-server protocol (`app_server`, served by `mhc app-server`) and
//! the SQLite session storage they share.

pub mod protocol;
pub mod client;
pub mod server;
pub mod app_server;
pub mod sqlite;
