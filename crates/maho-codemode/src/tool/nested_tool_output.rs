use crate::output::{
    streaming_output::DEFAULT_MAX_BYTES, streaming_output_buffer::truncate_tail_bytes,
};
use maho_ext_api::{AgentToolResult, ContentBlock};
use std::collections::BTreeMap;

/// Where a running cell receives the partial results of the tools it calls, so they stream live
/// (`#6167`).
pub trait CellToolUpdateSink: Send + Sync {
    fn nested_tool_update(&self, call_id: &str, partial: &AgentToolResult);
    fn nested_tool_settled(&self, call_id: &str);
}

/// The latest partial output of each tool call a cell is still awaiting. It joins the cell's live
/// view only: the value the tool returns to the cell and the cell's own result never contain it.
#[derive(Debug, Default)]
pub struct NestedToolOutput {
    by_call: BTreeMap<String, String>,
    /// Preserves insertion order for active call IDs so `append_to` outputs newest partials in the
    /// same order as upstream JS `Map` insertion order.
    order: Vec<String>,
    /// The newest settled call's last text, still on screen until something replaces it: the cell's
    /// own output, a live call's next text, a later settled call, or the cell's completion
    /// (`#6257`).
    held: Option<String>,
}

impl NestedToolOutput {
    /// Keeps the call's newest partial; `false` when it adds no new text.
    pub fn update(&mut self, call_id: &str, partial: &AgentToolResult) -> bool {
        let text = partial
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(content) => Some(content.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        if text.is_empty() || self.by_call.get(call_id).is_some_and(|prev| prev == &text) {
            return false;
        }
        if !self.by_call.contains_key(call_id) {
            self.order.push(call_id.to_owned());
        }
        self.by_call.insert(call_id.to_owned(), text);
        self.held = None;
        true
    }

    /// Marks a call settled; `false` when it never showed output. Its last text becomes the held
    /// text (replacing any earlier one) and stays in the live view until `release_settled` or
    /// `clear` (`#6257`).
    pub fn drop(&mut self, call_id: &str) -> bool {
        let Some(text) = self.by_call.remove(call_id) else {
            return false;
        };
        self.order.retain(|existing| existing != call_id);
        self.held = Some(text);
        true
    }

    /// Removes the held settled text; `false` when none was held.
    pub fn release_settled(&mut self) -> bool {
        self.held.take().is_some()
    }

    pub fn clear(&mut self) {
        self.by_call.clear();
        self.order.clear();
        self.held = None;
    }

    pub fn append_to(&self, output: &str) -> String {
        let mut combined = output.to_owned();
        let active = self
            .order
            .iter()
            .filter_map(|call_id| self.by_call.get(call_id).map(String::as_str));
        let texts: Vec<&str> = match &self.held {
            Some(held) => std::iter::once(held.as_str()).chain(active).collect(),
            None => active.collect(),
        };
        for text in texts {
            let tail = truncate_tail_bytes(text, DEFAULT_MAX_BYTES).text;
            if combined.is_empty() || combined.ends_with('\n') {
                combined.push_str(tail);
            } else {
                combined.push('\n');
                combined.push_str(tail);
            }
        }
        combined
    }

    /// Like `append_to`, kept to the last `max_lines` lines; `output` is returned untouched while no
    /// call shows output.
    pub fn append_lines(&self, output: &str, max_lines: usize) -> String {
        if self.by_call.is_empty() && self.held.is_none() {
            return output.to_owned();
        }
        let combined = self.append_to(output);
        let has_trailing_newline = combined.ends_with('\n');
        let mut lines: Vec<&str> = combined.split('\n').collect();
        if has_trailing_newline {
            lines.pop();
        }
        if max_lines == 0 {
            return if has_trailing_newline {
                "\n".to_owned()
            } else {
                String::new()
            };
        }
        let start = lines.len().saturating_sub(max_lines);
        let joined = lines[start..].join("\n");
        if has_trailing_newline {
            format!("{joined}\n")
        } else {
            joined
        }
    }
}
