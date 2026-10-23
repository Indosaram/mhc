//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "extension_watch_scope.rs"]
mod extension_watch_scope;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "generated_shim_filter.rs"]
mod generated_shim_filter;
#[path = "index.rs"]
mod index;
#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "log.rs"]
mod log;
#[path = "native_watch_engine.rs"]
mod native_watch_engine;
#[path = "protocol.rs"]
mod protocol;
#[path = "routine_settings.rs"]
mod routine_settings;
#[path = "session_scoped_callback.rs"]
mod session_scoped_callback;
#[path = "targets.rs"]
mod targets;
#[path = "watch_engine.rs"]
mod watch_engine;
#[path = "watch_event_source.rs"]
mod watch_event_source;
