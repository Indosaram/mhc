//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "client.rs"]
mod client;
#[path = "client_service_wire.rs"]
mod client_service_wire;
#[path = "client_unix.rs"]
mod client_unix;
#[path = "protocol_cbor.rs"]
mod protocol_cbor;
#[path = "protocol_codec.rs"]
mod protocol_codec;
#[path = "protocol_framing.rs"]
mod protocol_framing;
#[path = "server_listener.rs"]
mod server_listener;
#[path = "server_router.rs"]
mod server_router;
#[path = "server_state_codec.rs"]
mod server_state_codec;
#[path = "server_testing.rs"]
mod server_testing;
#[path = "server_unix.rs"]
mod server_unix;
#[path = "sqlite_branch_entries.rs"]
mod sqlite_branch_entries;
#[path = "sqlite_entries.rs"]
mod sqlite_entries;
#[path = "sqlite_repo.rs"]
mod sqlite_repo;
#[path = "sqlite_session_row.rs"]
mod sqlite_session_row;
#[path = "sqlite_sql.rs"]
mod sqlite_sql;
#[path = "sqlite_storage.rs"]
mod sqlite_storage;
#[path = "sqlite_values.rs"]
mod sqlite_values;
