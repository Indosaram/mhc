//! Port of senpi packages/ai/src/tool-call-middleware/recovery-text-projection.ts.

use crate::tool_call_middleware::protocols::anthropic_xml::recovery_stream::RecoveryStreamParser;
use crate::tool_call_middleware::protocols::antml::recovery_stream::create_antml_invoke_recovery_stream_parser;
use crate::tool_call_middleware::recovery_code_mask::{create_recovery_code_mask, RecoveryCodeMask, RecoveryCodeMaskFeedOptions, RecoveryCodeMaskSegment};
use crate::tool_call_middleware::recovery_diagnostics::append_recovery_diagnostic;
use crate::tool_call_middleware::recovery_native_projection::RecoveryNativeProjection;
use crate::tool_call_middleware::stream_wrapper_shared::StreamMessageProjection;
use crate::tool_call_middleware::types::{ParserOptions, StreamParserEvent, ToolCallFormat};
use crate::types::{AssistantMessage, Tool};

type CreateParserFn = Box<dyn Fn(Vec<Tool>) -> Box<dyn RecoveryStreamParser + Send> + Send>;

pub struct RecoveryTextProjectionOptions {
    pub create_parser: Option<CreateParserFn>,
    pub protocol: ToolCallFormat,
}

impl Default for RecoveryTextProjectionOptions {
    fn default() -> Self {
        Self { create_parser: None, protocol: ToolCallFormat::Antml }
    }
}

pub struct RecoveryTextProjection {
    inner_index: usize,
    parser: Box<dyn RecoveryStreamParser + Send>,
    protocol: ToolCallFormat,
    mask: RecoveryCodeMask,
    active_invoke: bool,
    text_buffer: String,
    saw_tool_call: bool,
    finished: bool,
}

impl RecoveryTextProjection {
    pub fn new(tools: Vec<Tool>, inner_index: usize, options: RecoveryTextProjectionOptions) -> Self {
        let parser = match options.create_parser {
            Some(create_parser) => create_parser(tools),
            None => create_antml_invoke_recovery_stream_parser(tools, None::<ParserOptions>),
        };
        Self { inner_index, parser, protocol: options.protocol, mask: create_recovery_code_mask(), active_invoke: false, text_buffer: String::new(), saw_tool_call: false, finished: false }
    }

    pub fn start(&self, projection: &mut StreamMessageProjection, native_projection: &mut RecoveryNativeProjection, source: &AssistantMessage) -> bool {
        let outer_index = projection.start_text(self.inner_index, source.content.get(self.inner_index));
        native_projection.start_text(self.inner_index, outer_index)
    }

    pub fn feed(&mut self, projection: &mut StreamMessageProjection, native_projection: &mut RecoveryNativeProjection, text: &str) -> bool {
        for character in text.chars() {
            let options = self.active_invoke.then_some(RecoveryCodeMaskFeedOptions { active_invoke: true });
            let segments = self.mask.feed(&character.to_string(), options);
            for segment in segments {
                self.process_segment(projection, native_projection, segment);
            }
        }
        self.flush_text(projection);
        self.saw_tool_call
    }

    pub fn finish(&mut self, projection: &mut StreamMessageProjection, native_projection: &mut RecoveryNativeProjection) -> bool {
        if self.finished {
            return self.saw_tool_call;
        }
        for segment in self.mask.finish() {
            self.process_segment(projection, native_projection, segment);
        }
        let events = self.parser.finish();
        self.project_parser_events(projection, native_projection, events);
        self.flush_text(projection);
        projection.finish_text();
        native_projection.extend_text(self.inner_index);
        self.finished = true;
        self.saw_tool_call
    }

    fn process_segment(&mut self, projection: &mut StreamMessageProjection, native_projection: &mut RecoveryNativeProjection, segment: RecoveryCodeMaskSegment) {
        if segment.recovery_boundary {
            let events = self.parser.interrupt();
            self.project_parser_events(projection, native_projection, events);
        }
        if segment.scan {
            let events = self.parser.feed(&segment.text);
            self.project_parser_events(projection, native_projection, events);
        } else {
            self.text_buffer.push_str(&segment.text);
        }
    }

    fn project_parser_events(&mut self, projection: &mut StreamMessageProjection, native_projection: &mut RecoveryNativeProjection, events: Vec<StreamParserEvent>) {
        for event in native_projection.assign_recovered_ids(events) {
            if let StreamParserEvent::Text { text } = event {
                self.text_buffer.push_str(&text);
                continue;
            }
            self.flush_text(projection);
            let is_start = matches!(event, StreamParserEvent::ToolcallStart { .. });
            let is_end = matches!(event, StreamParserEvent::ToolcallEnd { .. });
            let result = projection.project_parser_events(&[event]);
            self.saw_tool_call = self.saw_tool_call || result.saw_tool_call;
            if is_start {
                self.active_invoke = true;
            }
            if is_end {
                self.active_invoke = false;
                for tool_call in &result.completed_tool_calls {
                    append_recovery_diagnostic(projection, tool_call, self.protocol);
                }
            }
        }
        native_projection.extend_text(self.inner_index);
    }

    fn flush_text(&mut self, projection: &mut StreamMessageProjection) {
        if self.text_buffer.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.text_buffer);
        projection.project_parser_events(&[StreamParserEvent::Text { text }]);
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::tool_call_middleware::stream_wrapper_shared::StreamMessageProjectionOptions;
    use crate::types::{ContentBlock, StopReason, Usage};
    use crate::utils::event_stream::AssistantMessageEventStream;

    fn tool(name: &str) -> Tool {
        Tool { name: name.into(), description: "d".into(), parameters: json!({"type": "object"}), freeform: None, constrained_sampling: None }
    }

    fn message(content: Vec<ContentBlock>) -> AssistantMessage {
        AssistantMessage {
            content,
            api: "openai-completions".into(),
            provider: "openai".into(),
            model: "m".into(),
            response_model: None,
            response_id: None,
            provider_thinking_level: None,
            diagnostics: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            stop_details: None,
            deferred: None,
            error_message: None,
            abort_source: None,
            raw_stop_reason: None,
            end_turn: None,
            timestamp: 0,
        }
    }

    #[test]
    fn feed_recovers_a_leaked_invoke_and_marks_saw_tool_call() {
        let stream = AssistantMessageEventStream::assistant();
        let source = message(vec![]);
        let mut projection = StreamMessageProjection::new(stream.clone(), source.clone(), StreamMessageProjectionOptions::default());
        let mut native_projection = RecoveryNativeProjection::new(stream, message(vec![]), ToolCallFormat::Antml);
        let mut text_projection = RecoveryTextProjection::new(vec![tool("get_weather")], 0, RecoveryTextProjectionOptions::default());

        assert!(text_projection.start(&mut projection, &mut native_projection, &source));
        let saw = text_projection.feed(&mut projection, &mut native_projection, r#"<invoke name="get_weather"><parameter name="city">Seoul</parameter></invoke>"#);
        let finished = text_projection.finish(&mut projection, &mut native_projection);
        assert!(saw || finished);
    }
}
