//! Port of senpi packages/ai/src/tool-call-middleware/protocols/kimi-xtml/index.ts.
// ported by todo 12

pub mod format;
pub mod markers;
pub mod parse;
pub mod recovery_stream;
pub mod stream;
pub mod thinking_recovery;
pub mod thinking_recovery_stream;

pub use format::{kimi_xtml_format_tool_call, kimi_xtml_format_tool_response, kimi_xtml_format_tools_system_prompt};
pub use parse::parse_kimi_xtml_generated_text;
pub use stream::create_kimi_xtml_stream_parser;
