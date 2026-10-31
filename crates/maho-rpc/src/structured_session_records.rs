//! senpi `modes/rpc/structured-session-records.ts`: the session's internal structured lane behind the
//! compact-updates flag, and the seeded block gate every structured subscriber runs.
//!
//! Cross-lane: upstream reads the provider capture of a record (`getAssistantCapture`, maho-ai lane),
//! projects it with the seeded `toJsonEvent(record, {blockSeeds: 1})` (senpi-interactive-31, not yet in
//! `json_event.rs`) and subscribes through `AgentSession.subscribeStructured` (senpi-core-rest-58, not
//! yet in maho-core). The gate below is the whole upstream state machine, coded against those seams:
//! the caller supplies each record's `StructuredCapture` view and the seeded projection, so wiring
//! `subscribe_structured_session_records` to a live session is the remaining cross-lane step.
use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};

/// The engine-socket compact-updates rollout flag (`<brand>_ENGINE_SOCKET_COMPACT_UPDATES`), explicit
/// `1`/`true` only. Off, no session holds a structured subscription and every destination keeps its bytes.
pub fn structured_lane_enabled() -> bool {
    let env: HashMap<String, String> = std::env::vars().collect();
    matches!(
        maho_core::brand::env_value("ENGINE_SOCKET_COMPACT_UPDATES", &env).as_deref(),
        Some("1" | "true")
    )
}

/// The capture fields of one record the gate reads (upstream `getAssistantCapture(record)`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StructuredCapture {
    pub content_index: Option<usize>,
    pub offset: Option<usize>,
    pub next_offset: Option<usize>,
    /// Whether the terminal capture carries its frozen message.
    pub has_message: bool,
    pub rewritten_from: Option<usize>,
}

/// The seeded projection failed its frontier check (upstream `SeedFrontierError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeedFrontier;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockState {
    Open,
    Ended,
    Invalid,
}

/// Admits a block's records only after its own seed or insert reached this subscriber, so a subscriber
/// that attached mid-message gets no seed, delta or control for that message, never a wrong one. A
/// started block whose next record has no capture, or that has no accepted end when the message ends, is
/// invalid: its later records are dropped and the assistant `message_end` names it in `invalidatedBlocks`.
/// A terminal that rewrote blocks from `rewrittenFrom` on invalidates every block at or above that index,
/// and a `message_end` with no interpretable terminal capture invalidates every block of the message.
#[derive(Default)]
pub struct SeededBlockGate {
    blocks: Option<BTreeMap<usize, BlockState>>,
    next_offsets: HashMap<usize, usize>,
}

impl SeededBlockGate {
    /// `seed` is the seeded projection: `Ok(None)` when nothing was captured, `Err` at a frontier failure.
    pub fn accept(
        &mut self,
        record: &Value,
        capture: Option<&StructuredCapture>,
        seed: impl FnOnce() -> Result<Option<Value>, SeedFrontier>,
    ) -> Result<Option<Value>, SeedFrontier> {
        let kind = record
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let index_of = |record: &Value| {
            record
                .get("contentIndex")
                .and_then(Value::as_u64)
                .and_then(|n| usize::try_from(n).ok())
        };
        match kind {
            "message_block_state" | "message_block_metadata" => {
                let phase = (kind == "message_block_state")
                    .then(|| record.get("phase").and_then(Value::as_str))
                    .flatten();
                return Ok(self.control(record, index_of(record), phase));
            }
            "message_tool_arguments_reset" => {
                let accepted = self.control(record, index_of(record), None);
                if accepted.is_some()
                    && let Some(index) = index_of(record)
                {
                    let prefix = record["rawArgumentPrefix"].as_str().unwrap_or_default();
                    self.next_offsets
                        .insert(index, prefix.encode_utf16().count());
                }
                return Ok(accepted);
            }
            _ => {}
        }
        let assistant = record["message"]["role"].as_str() == Some("assistant");
        if kind == "message_start" && assistant {
            let seeded = seed()?;
            self.blocks = seeded.as_ref().map(|_| BTreeMap::new());
            self.next_offsets.clear();
            return Ok(seeded);
        }
        if kind == "message_end" && assistant {
            return Ok(Some(self.end(record, capture, seed)?));
        }
        if kind != "message_update" {
            return seed();
        }
        if self.blocks.is_none() {
            return Ok(None);
        }
        let update = &record["assistantMessageEvent"];
        let Some(public_index) = index_of(update) else {
            return seed();
        };
        let index = capture
            .and_then(|capture| capture.content_index)
            .unwrap_or(public_index);
        let update_type = update["type"].as_str().unwrap_or_default();
        let starts = matches!(
            update_type,
            "text_start" | "thinking_start" | "toolcall_start"
        );
        let blocks = self.blocks.get_or_insert_with(BTreeMap::new);
        if starts && self.next_offsets.contains_key(&index) {
            blocks.insert(index, BlockState::Invalid);
            return Ok(None);
        }
        let state = blocks.get(&index).copied();
        if state == Some(BlockState::Invalid) || (!starts && state != Some(BlockState::Open)) {
            return Ok(None);
        }
        let seeded = match seed() {
            Ok(seeded) => seeded,
            Err(SeedFrontier) => {
                blocks.insert(index, BlockState::Invalid);
                return Ok(None);
            }
        };
        if starts
            && seeded.is_some()
            && let Some(next) = capture.and_then(|capture| capture.next_offset)
        {
            self.next_offsets.insert(index, next);
        }
        if let Some(delta) = update.get("delta").and_then(Value::as_str)
            && seeded.is_some()
        {
            let offset = capture.and_then(|capture| capture.offset);
            if offset.is_none() || offset != self.next_offsets.get(&index).copied() {
                blocks.insert(index, BlockState::Invalid);
                return Ok(None);
            }
            self.next_offsets
                .insert(index, offset.unwrap_or(0) + delta.encode_utf16().count());
        }
        let ends = matches!(update_type, "text_end" | "thinking_end" | "toolcall_end");
        blocks.insert(
            index,
            match (&seeded, ends) {
                (None, _) => BlockState::Invalid,
                (Some(_), true) => BlockState::Ended,
                (Some(_), false) => BlockState::Open,
            },
        );
        Ok(seeded)
    }

    fn control(
        &mut self,
        record: &Value,
        index: Option<usize>,
        phase: Option<&str>,
    ) -> Option<Value> {
        let blocks = self.blocks.as_mut()?;
        let index = index?;
        let state = blocks.get(&index).copied();
        if state == Some(BlockState::Invalid) {
            return None;
        }
        match phase {
            Some("insert") => {
                blocks.insert(index, BlockState::Open);
            }
            _ if state.is_none() || (phase == Some("end") && state != Some(BlockState::Open)) => {
                return None;
            }
            Some("end") => {
                blocks.insert(index, BlockState::Ended);
            }
            _ => {}
        }
        Some(record.clone())
    }

    fn end(
        &mut self,
        record: &Value,
        capture: Option<&StructuredCapture>,
        seed: impl FnOnce() -> Result<Option<Value>, SeedFrontier>,
    ) -> Result<Value, SeedFrontier> {
        let rewritten_from = match capture {
            Some(capture) if capture.has_message => capture.rewritten_from.unwrap_or(usize::MAX),
            _ => 0,
        };
        let invalidated: Vec<usize> = self
            .blocks
            .take()
            .unwrap_or_default()
            .into_iter()
            .filter(|(index, state)| !(*state == BlockState::Ended && *index < rewritten_from))
            .map(|(index, _)| index)
            .collect();
        self.next_offsets.clear();
        let mut seeded = seed()?.unwrap_or_else(|| record.clone());
        if !invalidated.is_empty()
            && let Some(object) = seeded.as_object_mut()
        {
            object.insert("invalidatedBlocks".into(), json!(invalidated));
        }
        Ok(seeded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update(kind: &str, index: usize, delta: Option<&str>) -> Value {
        let mut event = json!({"type":kind,"contentIndex":index});
        if let Some(delta) = delta {
            event["delta"] = json!(delta);
        }
        json!({"type":"message_update","message":{"role":"assistant"},"assistantMessageEvent":event})
    }

    fn cap(offset: Option<usize>, next: Option<usize>) -> StructuredCapture {
        StructuredCapture {
            offset,
            next_offset: next,
            ..StructuredCapture::default()
        }
    }

    fn pass(record: &Value) -> impl FnOnce() -> Result<Option<Value>, SeedFrontier> + '_ {
        move || Ok(Some(record.clone()))
    }

    // senpi 4272-structured-lane-validity: offset gaps invalidate a block and the terminal names it.
    #[test]
    fn invalidates_gapped_blocks_and_names_them_at_the_end() {
        let mut gate = SeededBlockGate::default();
        let start = json!({"type":"message_start","message":{"role":"assistant"}});
        assert!(
            gate.accept(&start, None, pass(&start))
                .expect("start")
                .is_some()
        );
        for (index, records) in [
            (
                0,
                vec![
                    (update("text_start", 0, None), cap(None, Some(0))),
                    (update("text_delta", 0, Some("ab")), cap(Some(0), None)),
                    (update("text_end", 0, None), cap(None, None)),
                ],
            ),
            (
                1,
                vec![
                    (update("text_start", 1, None), cap(None, Some(0))),
                    (update("text_delta", 1, Some("x")), cap(Some(5), None)),
                ],
            ),
        ] {
            for (record, capture) in records {
                let _ = gate
                    .accept(&record, Some(&capture), pass(&record))
                    .expect("update");
            }
            let _ = index;
        }
        let late = update("text_delta", 1, Some("y"));
        assert!(
            gate.accept(&late, Some(&cap(Some(1), None)), pass(&late))
                .expect("late")
                .is_none()
        );
        let end = json!({"type":"message_end","message":{"role":"assistant"}});
        let terminal = StructuredCapture {
            has_message: true,
            ..StructuredCapture::default()
        };
        let out = gate
            .accept(&end, Some(&terminal), pass(&end))
            .expect("end")
            .expect("record");
        assert_eq!(out["invalidatedBlocks"], json!([1]));
    }

    #[test]
    fn a_mid_message_subscriber_gets_nothing_for_that_message() {
        let mut gate = SeededBlockGate::default();
        let delta = update("text_delta", 0, Some("a"));
        assert!(
            gate.accept(&delta, Some(&cap(Some(0), None)), pass(&delta))
                .expect("delta")
                .is_none()
        );
        let control = json!({"type":"message_block_state","contentIndex":0,"phase":"end"});
        assert!(
            gate.accept(&control, None, pass(&control))
                .expect("control")
                .is_none()
        );
        let end = json!({"type":"message_end","message":{"role":"assistant"}});
        let out = gate
            .accept(&end, None, pass(&end))
            .expect("end")
            .expect("record");
        assert!(out.get("invalidatedBlocks").is_none());
    }

    #[test]
    fn a_terminal_without_capture_invalidates_every_block() {
        let mut gate = SeededBlockGate::default();
        let start = json!({"type":"message_start","message":{"role":"assistant"}});
        let _ = gate.accept(&start, None, pass(&start));
        let first = update("text_start", 2, None);
        let _ = gate.accept(&first, Some(&cap(None, Some(0))), pass(&first));
        let end_block = update("text_end", 2, None);
        let _ = gate.accept(&end_block, Some(&cap(None, None)), pass(&end_block));
        let end = json!({"type":"message_end","message":{"role":"assistant"}});
        let out = gate
            .accept(&end, None, || Ok(None))
            .expect("end")
            .expect("record");
        assert_eq!(out["invalidatedBlocks"], json!([2]));
    }
}
