//! Port of senpi packages/ai/src/tool-call-middleware/types.ts.

use std::collections::HashMap;

use serde_json::Value;

use crate::types::{ImageContent, TextContent, Tool};

/// Supported tool call formats for models that don't natively support tool calling.
/// - "hermes": Hermes 2/3 format with special delimiters
/// - "morph-xml": XML-based tool call format
/// - "xml": Deprecated alias for "morph-xml"
/// - "yaml-xml": YAML arguments inside XML tool tags
/// - "gemma4-delimiter": Gemma 4 specific delimiter format
/// - "anthropic-xml": Legacy Anthropic invoke/parameter XML format
/// - "antml": ANTML function_calls/invoke format with Claude-Code-style failure tolerance
/// - "kimi-xtml": Kimi K3 XTML channel format (<|open|>/<|close|>/<|sep|> markers)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolCallFormat {
    Hermes,
    /// Deprecated alias for `MorphXml`.
    Xml,
    MorphXml,
    YamlXml,
    Gemma4Delimiter,
    AnthropicXml,
    Antml,
    KimiXtml,
}

impl ToolCallFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolCallFormat::Hermes => "hermes",
            ToolCallFormat::Xml => "xml",
            ToolCallFormat::MorphXml => "morph-xml",
            ToolCallFormat::YamlXml => "yaml-xml",
            ToolCallFormat::Gemma4Delimiter => "gemma4-delimiter",
            ToolCallFormat::AnthropicXml => "anthropic-xml",
            ToolCallFormat::Antml => "antml",
            ToolCallFormat::KimiXtml => "kimi-xtml",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "hermes" => Some(ToolCallFormat::Hermes),
            "xml" => Some(ToolCallFormat::Xml),
            "morph-xml" => Some(ToolCallFormat::MorphXml),
            "yaml-xml" => Some(ToolCallFormat::YamlXml),
            "gemma4-delimiter" => Some(ToolCallFormat::Gemma4Delimiter),
            "anthropic-xml" => Some(ToolCallFormat::AnthropicXml),
            "antml" => Some(ToolCallFormat::Antml),
            "kimi-xtml" => Some(ToolCallFormat::KimiXtml),
            _ => None,
        }
    }
}

/// Content type for tool results (text or image). `TextContent | ImageContent`.
#[derive(Debug, Clone, PartialEq)]
pub enum ToolResultContent {
    Text(TextContent),
    Image(ImageContent),
}

impl ToolResultContent {
    pub fn as_text(&self) -> Option<&TextContent> {
        match self {
            ToolResultContent::Text(text) => Some(text),
            ToolResultContent::Image(_) => None,
        }
    }
}

#[derive(Clone, Default)]
pub struct ParserOptions {
    pub emit_raw_tool_call_text_on_error: bool,
    /// `onError?: (message: string, metadata?: Record<string, unknown>) => void`.
    pub on_error: Option<std::sync::Arc<dyn Fn(&str, Option<&HashMap<String, Value>>) + Send + Sync>>,
}

impl std::fmt::Debug for ParserOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ParserOptions")
            .field("emit_raw_tool_call_text_on_error", &self.emit_raw_tool_call_text_on_error)
            .field("on_error", &self.on_error.as_ref().map(|_| "<fn>"))
            .finish()
    }
}

impl ParserOptions {
    pub fn report_error(&self, message: &str, metadata: Option<HashMap<String, Value>>) {
        if let Some(on_error) = &self.on_error {
            on_error(message, metadata.as_ref());
        }
    }
}

/// A parsed tool call extracted from generated text.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedToolCall {
    pub name: String,
    pub arguments: serde_json::Map<String, Value>,
}

/// Events emitted by the stream parser during text processing.
/// Maps 1:1 to `AssistantMessageEvent`s for seamless integration.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamParserEvent {
    Text { text: String },
    ToolcallStart { index: usize, name: String, id: String },
    ToolcallDelta { index: usize, arguments_delta: String },
    ToolcallEnd {
        index: usize,
        name: String,
        id: String,
        arguments: serde_json::Map<String, Value>,
        incomplete: bool,
        error_message: Option<String>,
    },
}

/// Stream parser for incremental tool call parsing during streaming.
pub trait StreamParser {
    /// Feed a text delta into the parser and receive any events generated.
    fn feed(&mut self, text_delta: &str) -> Vec<StreamParserEvent>;

    /// Signal that the stream has ended and receive any final events.
    fn finish(&mut self) -> Vec<StreamParserEvent>;
}

/// Protocol interface for formatting and parsing tool calls in different formats.
/// Implementations handle the specifics of each tool call format (Hermes, XML, etc.)
pub trait ToolCallProtocol {
    /// Format tools into a system prompt that instructs the model how to use tools.
    fn format_tools_system_prompt(&self, tools: &[Tool]) -> String;

    /// Format a tool response for inclusion in the conversation context.
    fn format_tool_response(&self, tool_name: &str, tool_call_id: &str, content: &[ToolResultContent]) -> String;

    /// Format a tool call for sending to the model.
    fn format_tool_call(&self, name: &str, args: &serde_json::Map<String, Value>) -> String;

    /// Parse generated text to extract tool calls.
    fn parse_generated_text(&self, text: &str, tools: &[Tool], options: Option<&ParserOptions>) -> Vec<ParsedToolCall>;

    /// Create a stream parser for incremental parsing during streaming.
    fn create_stream_parser(&self, tools: Vec<Tool>, options: Option<ParserOptions>) -> Box<dyn StreamParser + Send>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_format_round_trips_every_variant() {
        for (format, text) in [
            (ToolCallFormat::Hermes, "hermes"),
            (ToolCallFormat::Xml, "xml"),
            (ToolCallFormat::MorphXml, "morph-xml"),
            (ToolCallFormat::YamlXml, "yaml-xml"),
            (ToolCallFormat::Gemma4Delimiter, "gemma4-delimiter"),
            (ToolCallFormat::AnthropicXml, "anthropic-xml"),
            (ToolCallFormat::Antml, "antml"),
            (ToolCallFormat::KimiXtml, "kimi-xtml"),
        ] {
            assert_eq!(format.as_str(), text);
            assert_eq!(ToolCallFormat::parse(text), Some(format));
        }
        assert_eq!(ToolCallFormat::parse("unknown"), None);
    }
}
