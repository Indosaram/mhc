//! Port of senpi packages/ai/src/tool-call-middleware/protocols/anthropic-xml/index.ts.
// ported by todo 12

pub mod coerce_parameters;
pub mod format;
pub mod invoke_match;
pub mod invoke_protocol;
pub mod invoke_stream_helpers;
pub mod invoke_tag_scanner;
pub mod invoke_tag_syntax;
pub mod parse;
pub mod recovery_stream;
pub mod recovery_wrapper_state;
pub mod stream;
pub mod stream_boundary;
pub mod tool_resolver;
pub mod xml_entities;

pub use format::{anthropic_xml_format_tool_call, anthropic_xml_format_tool_response, anthropic_xml_format_tools_system_prompt};
pub use parse::parse_anthropic_xml_generated_text;
pub use stream::create_anthropic_xml_stream_parser;
