//! Port of senpi packages/ai/src (index.ts): the maho-ai unified LLM API.

pub mod api;
pub mod api_registry;
pub mod auth;
pub mod bedrock_provider;
pub mod bun_oauth;
pub mod cli;
pub mod compat;
pub mod context_provenance;
pub mod cursor;
pub mod cursor_agent_provider;
pub mod devin_provider;
pub mod env_api_keys;
pub mod image_models;
pub mod image_models_generated;
pub mod images;
pub mod images_api_registry;
pub mod images_models;
pub mod legacy_api_aliases;
pub mod legacy_provider_ids;
pub mod model;
pub mod model_catalog;
pub mod models;
pub mod models_store;
pub mod models_generated;
pub mod node;
pub mod oauth;
pub mod openai_responses_compat;
pub mod providers;
pub mod session_resources;
pub mod stream;
pub mod tool_call_middleware;
pub mod types;
pub mod utils;
pub mod wire_identity;

// index.ts re-exports (tool-call middleware activation helpers), owned by todo 12.
pub use tool_call_middleware::{get_tool_call_format, has_kimi_text_tool_call_recovery, should_recover_text_tool_calls, wrap_stream_with_model_recovery};
