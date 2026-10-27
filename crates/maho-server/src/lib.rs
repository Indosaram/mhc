//! Shared-host layer for `mhc`: the daemon RPC protocol frames (`protocol`), its
//! client and server, and the SQLite session storage they share.

pub mod client;
pub mod protocol;
pub mod server;
pub mod sqlite;
