//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "credential_store_contract.rs"]
mod credential_store_contract;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "oauth_login.rs"]
mod oauth_login;
#[path = "reconciled_recovery.rs"]
mod reconciled_recovery;
#[path = "request_env.rs"]
mod request_env;
#[path = "session_stream.rs"]
mod session_stream;
#[path = "stream.rs"]
mod stream;
#[path = "toolless_summary.rs"]
mod toolless_summary;
