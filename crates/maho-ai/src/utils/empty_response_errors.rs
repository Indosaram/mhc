//! Port of senpi packages/ai/src/utils/empty-response-errors.ts.

pub const EMPTY_RESPONSE_ERROR: &str = "Model returned an empty response twice";
pub const EMPTY_TOOL_USE_ERROR: &str = "Model returned tool_use without a tool call twice";
pub const FORWARDED_EMPTY_RESPONSE_ERROR: &str = "Model returned an empty response after streaming thinking";
pub const FORWARDED_EMPTY_TOOL_USE_ERROR: &str = "Model returned tool_use without a tool call after streaming thinking";
