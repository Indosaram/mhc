//! Settings-configured lifecycle command hooks.
pub mod diagnostics;
pub mod config_loader;
pub mod command_runner;
pub mod handler;
pub mod matcher;
pub mod output_parser;
pub mod output_bounds;
pub mod schema;
pub mod safety;
pub mod plugin_manifest;
pub mod plugin_loader;
pub mod trust;
pub mod trust_state_json;
pub mod trust_storage;
pub mod types;
