//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "config_source_cases.rs"]
mod config_source_cases;
#[path = "endpoint_source_cases.rs"]
mod endpoint_source_cases;
#[path = "native_execute.rs"]
mod native_execute;
#[path = "native_registry.rs"]
mod native_registry;
#[path = "native_render.rs"]
mod native_render;
#[path = "provider_source_config.rs"]
mod provider_source_config;
