//! Port of senpi packages/ai/src/tool-call-middleware/recovery-native-projection.ts.

use std::collections::{HashMap, HashSet};

use crate::types::{AssistantMessage, AssistantMessageEvent, ContentBlock, ToolCall};
use crate::utils::assistant_capture::{FrozenProgress, push_assistant_progress};
use crate::utils::event_stream::AssistantMessageEventStream;

use super::types::{StreamParserEvent, ToolCallFormat};

#[derive(Clone, Copy)]
struct ContentRange {
    start: usize,
    end: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NativeLifecycle {
    Started,
    Ended,
}

pub enum ProjectNativeStartResult {
    Projected,
    Collision,
    Invalid,
}

fn clone_content_block(block: &ContentBlock) -> ContentBlock {
    block.clone()
}

/// Maintains source-index order and native/recovered ID ownership for recovery streams.
///
/// senpi constructs this with `projection.message` and mutates that same object, so every block
/// it projects lands in the message the terminal finalizes. Rust cannot alias that, so the
/// projection holds no message of its own: each call that reads or writes content takes the
/// owning `StreamMessageProjection.message` by reference.
pub struct RecoveryNativeProjection {
    stream: AssistantMessageEventStream,
    ranges_by_inner_index: HashMap<usize, ContentRange>,
    copied_inner_indexes: HashSet<usize>,
    pending_progress: HashMap<usize, Vec<FrozenProgress>>,
    native_lifecycle_by_inner_index: HashMap<usize, NativeLifecycle>,
    reserved_ids: HashSet<String>,
    recovered_ids: HashSet<String>,
    recovered_id_by_parser_index: HashMap<usize, String>,
    recovered_id_prefix: String,
    highest_projected_inner_index: i64,
    next_recovered_id: u64,
}

impl RecoveryNativeProjection {
    pub fn new(stream: AssistantMessageEventStream, protocol: ToolCallFormat) -> Self {
        Self {
            stream,
            ranges_by_inner_index: HashMap::new(),
            copied_inner_indexes: HashSet::new(),
            pending_progress: HashMap::new(),
            native_lifecycle_by_inner_index: HashMap::new(),
            reserved_ids: HashSet::new(),
            recovered_ids: HashSet::new(),
            recovered_id_by_parser_index: HashMap::new(),
            recovered_id_prefix: format!("recovered-{}-", protocol.as_str()),
            highest_projected_inner_index: -1,
            next_recovered_id: 0,
        }
    }

    pub fn reserve_visible_ids(&mut self, source: &AssistantMessage) {
        for block in &source.content {
            if let ContentBlock::ToolCall(tool_call) = block {
                self.reserved_ids.insert(tool_call.id.clone());
            }
        }
    }

    pub fn synchronize_lower(
        &mut self,
        message: &mut AssistantMessage,
        source: &AssistantMessage,
        content_index: usize,
    ) -> bool {
        let bound = content_index.min(source.content.len());
        for inner_index in 0..bound {
            if self.ranges_by_inner_index.contains_key(&inner_index) {
                continue;
            }
            if !self.append_unannounced(message, source, inner_index) {
                return false;
            }
        }
        true
    }

    pub fn synchronize_remaining(
        &mut self,
        message: &mut AssistantMessage,
        source: &AssistantMessage,
    ) -> bool {
        for inner_index in 0..source.content.len() {
            if self.ranges_by_inner_index.contains_key(&inner_index) {
                continue;
            }
            if !self.append_unannounced(message, source, inner_index) {
                return false;
            }
        }
        true
    }

    pub fn start_text(
        &mut self,
        message: &AssistantMessage,
        inner_index: usize,
        outer_index: usize,
    ) -> bool {
        let end = message.content.len();
        self.record_range(
            inner_index,
            ContentRange {
                start: outer_index,
                end,
            },
            false,
        )
    }

    pub fn record_projected_block(&mut self, inner_index: usize, outer_index: usize) -> bool {
        self.record_range(
            inner_index,
            ContentRange {
                start: outer_index,
                end: outer_index + 1,
            },
            true,
        )
    }

    /// Relays a source control to the outer block that copies its source block. Controls of a
    /// block the projection rebuilds (prose split into text and recovered calls) describe no
    /// outer block and are dropped; controls of a block not projected yet wait for it.
    pub fn relay_progress(&mut self, progress: FrozenProgress) {
        let content_index = progress.content_index();
        let Some(range) = self.ranges_by_inner_index.get(&content_index).copied() else {
            self.pending_progress
                .entry(content_index)
                .or_default()
                .push(progress);
            return;
        };
        if !self.copied_inner_indexes.contains(&content_index) {
            return;
        }
        if range.start == content_index {
            push_assistant_progress(&self.stream, progress);
        } else {
            push_assistant_progress(
                &self.stream,
                std::sync::Arc::new(progress.with_content_index(range.start)),
            );
        }
    }

    pub fn extend_text(&mut self, message: &AssistantMessage, inner_index: usize) {
        let new_end = message.content.len();
        if let Some(range) = self.ranges_by_inner_index.get_mut(&inner_index) {
            range.end = new_end;
        }
    }

    pub fn project_native_start(
        &mut self,
        message: &mut AssistantMessage,
        source: &AssistantMessage,
        inner_index: usize,
    ) -> ProjectNativeStartResult {
        if self
            .native_lifecycle_by_inner_index
            .contains_key(&inner_index)
            || self.ranges_by_inner_index.contains_key(&inner_index)
        {
            return ProjectNativeStartResult::Invalid;
        }
        let Some(block) = source.content.get(inner_index) else {
            return ProjectNativeStartResult::Invalid;
        };
        let ContentBlock::ToolCall(tool_call) = block else {
            return ProjectNativeStartResult::Invalid;
        };
        if inner_index as i64 <= self.highest_projected_inner_index {
            return ProjectNativeStartResult::Invalid;
        }
        if self.recovered_ids.contains(&tool_call.id) {
            return ProjectNativeStartResult::Collision;
        }
        self.reserved_ids.insert(tool_call.id.clone());
        let outer_index = message.content.len();
        message.content.push(clone_content_block(block));
        self.native_lifecycle_by_inner_index
            .insert(inner_index, NativeLifecycle::Started);
        self.stream.push(AssistantMessageEvent::ToolcallStart {
            content_index: outer_index,
            partial: message.clone(),
        });
        // Recorded after the start, as a thinking copy is, so controls held for the call follow
        // its start event. The guards above already rule out both reasons record_range refuses.
        if self.record_range(
            inner_index,
            ContentRange {
                start: outer_index,
                end: outer_index + 1,
            },
            true,
        ) {
            ProjectNativeStartResult::Projected
        } else {
            ProjectNativeStartResult::Invalid
        }
    }

    pub fn project_native_delta(
        &mut self,
        message: &mut AssistantMessage,
        source: &AssistantMessage,
        inner_index: usize,
        delta: &str,
    ) -> bool {
        if self.native_lifecycle_by_inner_index.get(&inner_index) != Some(&NativeLifecycle::Started)
        {
            return false;
        }
        let Some(outer_index) = self
            .ranges_by_inner_index
            .get(&inner_index)
            .map(|range| range.start)
        else {
            return false;
        };
        let Some(block) = source.content.get(inner_index) else {
            return false;
        };
        if !matches!(block, ContentBlock::ToolCall(_)) {
            return false;
        }
        let Some(slot) = message.content.get_mut(outer_index) else {
            return false;
        };
        *slot = clone_content_block(block);
        self.stream.push(AssistantMessageEvent::ToolcallDelta {
            content_index: outer_index,
            delta: delta.to_string(),
            partial: message.clone(),
        });
        true
    }

    pub fn project_native_end(
        &mut self,
        message: &mut AssistantMessage,
        inner_index: usize,
        tool_call: ToolCall,
    ) -> bool {
        if self.native_lifecycle_by_inner_index.get(&inner_index) != Some(&NativeLifecycle::Started)
        {
            return false;
        }
        let Some(outer_index) = self
            .ranges_by_inner_index
            .get(&inner_index)
            .map(|range| range.start)
        else {
            return false;
        };
        let Some(slot) = message.content.get_mut(outer_index) else {
            return false;
        };
        *slot = ContentBlock::ToolCall(tool_call.clone());
        self.native_lifecycle_by_inner_index
            .insert(inner_index, NativeLifecycle::Ended);
        self.stream.push(AssistantMessageEvent::ToolcallEnd {
            content_index: outer_index,
            tool_call,
            partial: message.clone(),
        });
        true
    }

    pub fn assign_recovered_ids(
        &mut self,
        events: Vec<StreamParserEvent>,
    ) -> Vec<StreamParserEvent> {
        events
            .into_iter()
            .map(|event| match event {
                StreamParserEvent::ToolcallStart { index, name, .. } => {
                    let id = self.allocate_recovered_id();
                    self.recovered_id_by_parser_index.insert(index, id.clone());
                    StreamParserEvent::ToolcallStart { index, name, id }
                }
                StreamParserEvent::ToolcallEnd {
                    index,
                    name,
                    id,
                    arguments,
                    incomplete,
                    error_message,
                } => {
                    let resolved_id = self
                        .recovered_id_by_parser_index
                        .remove(&index)
                        .unwrap_or(id);
                    StreamParserEvent::ToolcallEnd {
                        index,
                        name,
                        id: resolved_id,
                        arguments,
                        incomplete,
                        error_message,
                    }
                }
                other => other,
            })
            .collect()
    }

    fn append_unannounced(
        &mut self,
        message: &mut AssistantMessage,
        source: &AssistantMessage,
        inner_index: usize,
    ) -> bool {
        let Some(block) = source.content.get(inner_index) else {
            return false;
        };
        if inner_index as i64 <= self.highest_projected_inner_index {
            return false;
        }
        if let ContentBlock::ToolCall(tool_call) = block
            && self.recovered_ids.contains(&tool_call.id)
        {
            return false;
        }
        let outer_index = message.content.len();
        message.content.push(clone_content_block(block));
        self.record_range(
            inner_index,
            ContentRange {
                start: outer_index,
                end: outer_index + 1,
            },
            true,
        )
    }

    /// Flushes the controls held for `inner_index`; a copied block with a start event records its
    /// range after that start.
    fn record_range(
        &mut self,
        inner_index: usize,
        range: ContentRange,
        copies_source: bool,
    ) -> bool {
        if self.ranges_by_inner_index.contains_key(&inner_index)
            || inner_index as i64 <= self.highest_projected_inner_index
        {
            return false;
        }
        self.ranges_by_inner_index.insert(inner_index, range);
        self.highest_projected_inner_index = inner_index as i64;
        if copies_source {
            self.copied_inner_indexes.insert(inner_index);
        }
        for progress in self
            .pending_progress
            .remove(&inner_index)
            .unwrap_or_default()
        {
            self.relay_progress(progress);
        }
        true
    }

    fn allocate_recovered_id(&mut self) -> String {
        loop {
            let id = format!("{}{}", self.recovered_id_prefix, self.next_recovered_id);
            self.next_recovered_id += 1;
            if self.reserved_ids.contains(&id) {
                continue;
            }
            self.reserved_ids.insert(id.clone());
            self.recovered_ids.insert(id.clone());
            return id;
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Map;

    use super::*;
    use crate::types::{StopReason, TextContent, Usage};

    fn message(content: Vec<ContentBlock>) -> AssistantMessage {
        AssistantMessage {
            error_cause: None,
            prompt_cache_breakpoint: None,
            prompt_cache_diagnostics: None,
            thinking_level: None,
            provider_diagnostic: None,
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
            failure_source: None,
            credential_served_recently: None,
            timing: None,
        }
    }

    fn tool_call(id: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: "get_weather".into(),
            arguments: Map::new(),
            incomplete: None,
            error_message: None,
            thought_signature: None,
            namespace: None,
        }
    }

    #[test]
    fn project_native_start_projects_and_flags_collisions() {
        let stream = AssistantMessageEventStream::assistant();
        let source = message(vec![ContentBlock::ToolCall(tool_call("id-1"))]);
        let mut outer = message(vec![]);
        let mut projection = RecoveryNativeProjection::new(stream, ToolCallFormat::Antml);
        assert!(matches!(
            projection.project_native_start(&mut outer, &source, 0),
            ProjectNativeStartResult::Projected
        ));
        assert!(matches!(
            projection.project_native_start(&mut outer, &source, 0),
            ProjectNativeStartResult::Invalid
        ));
        assert!(
            matches!(outer.content.as_slice(), [ContentBlock::ToolCall(call)] if call.id == "id-1")
        );
    }

    #[test]
    fn assign_recovered_ids_reuses_the_start_id_for_the_matching_end() {
        let stream = AssistantMessageEventStream::assistant();
        let mut projection = RecoveryNativeProjection::new(stream, ToolCallFormat::Antml);
        let events = vec![
            StreamParserEvent::ToolcallStart {
                index: 0,
                name: "t".into(),
                id: "placeholder".into(),
            },
            StreamParserEvent::ToolcallEnd {
                index: 0,
                name: "t".into(),
                id: "placeholder".into(),
                arguments: Map::new(),
                incomplete: false,
                error_message: None,
            },
        ];
        let assigned = projection.assign_recovered_ids(events);
        let StreamParserEvent::ToolcallStart { id: start_id, .. } = &assigned[0] else {
            panic!("expected start")
        };
        let StreamParserEvent::ToolcallEnd { id: end_id, .. } = &assigned[1] else {
            panic!("expected end")
        };
        assert_eq!(start_id, end_id);
        assert!(start_id.starts_with("recovered-antml-"));
    }

    #[test]
    fn synchronize_remaining_appends_every_unannounced_block_in_order() {
        let stream = AssistantMessageEventStream::assistant();
        let source = message(vec![ContentBlock::Text(TextContent {
            text: "hi".into(),
            audience: None,
            text_signature: None,
        })]);
        let mut outer = message(vec![]);
        let mut projection = RecoveryNativeProjection::new(stream, ToolCallFormat::Antml);
        assert!(projection.synchronize_remaining(&mut outer, &source));
        assert_eq!(outer.content.len(), 1);
    }
}
