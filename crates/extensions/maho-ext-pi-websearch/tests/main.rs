//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "config.rs"]
mod config;
#[path = "index.rs"]
mod index;
#[path = "native.rs"]
mod native;
#[path = "provider_core.rs"]
mod provider_core;
#[path = "provider_endpoints.rs"]
mod provider_endpoints;
#[path = "providers.rs"]
mod providers;
#[path = "registered.rs"]
mod registered;
#[path = "renderers.rs"]
mod renderers;
#[path = "routing.rs"]
mod routing;
#[path = "search.rs"]
mod search;
#[path = "tool.rs"]
mod tool;
