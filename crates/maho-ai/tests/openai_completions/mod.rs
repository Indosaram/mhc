//! Ported `packages/ai/test/openai-completions-*.test.ts` cases.

pub mod harness;

mod cache_control_format;
mod empty_tools;
mod freeform_guard;
mod prompt_cache;
mod raw_stop_reason;
mod response_model;
mod retry;
mod stream_lifecycle;
mod thinking_as_text;
mod thinking_matrix;
mod thinking_token_budget;
mod tool_result_images;
mod tool_schema_compat;
mod tool_choice;
mod vllm_priority;
