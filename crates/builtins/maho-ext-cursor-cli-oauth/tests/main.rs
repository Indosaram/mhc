//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "faux_session.rs"]
mod faux_session;
#[path = "models_probe.rs"]
mod models_probe;
#[path = "oauth_adapter.rs"]
mod oauth_adapter;
#[path = "stream.rs"]
mod stream;
#[path = "stream_attempt.rs"]
mod stream_attempt;
#[path = "transport.rs"]
mod transport;
