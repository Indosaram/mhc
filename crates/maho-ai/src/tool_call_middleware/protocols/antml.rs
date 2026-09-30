//! Port of senpi packages/ai/src/tool-call-middleware/protocols/antml/index.ts.
// ported by todo 12

pub mod coerce_parameters;
pub mod config;
pub mod format;
pub mod parse;
pub mod recovery_stream;
pub mod repair;
pub mod stream;

pub use format::{antml_format_tool_call, antml_format_tool_response, antml_format_tools_system_prompt};
pub use parse::parse_antml_generated_text;
pub use stream::create_antml_stream_parser;
