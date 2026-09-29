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
