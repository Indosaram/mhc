//! senpi `modes/rpc/compact-record-staging.ts`: ordinary records and host snapshots share the
//! bounded fragment/staging substrate. Also hosts iterative `json_chunks` and `json_size`
//! (from `json-pieces.ts`) so `compact_materializer_line` and `compact_status_snapshot` share one
//! non-recursive implementation.

use crate::compact_content_store::utf16_len;
use crate::compact_recovery_bytes::FRAGMENT_MAX_BYTES;
use crate::compact_recovery_ids::protocol_id;
use crate::compact_transfer_staging::{StagedContent, StagingOpError, TransferStaging};
use serde_json::Value;

pub enum RecordContent {
    Text(String),
    Units(usize),
    Chunks(Vec<String>),
}

enum JsonFrame<'a> {
    Array {
        items: &'a [Value],
        index: usize,
    },
    Object {
        entries: Vec<(&'a String, &'a Value)>,
        index: usize,
    },
}

fn serialize_piece_into(value: &Value, mut emit: impl FnMut(&str)) {
    let mut stack: Vec<JsonFrame<'_>> = Vec::new();
    let mut pending: Option<&Value> = Some(value);
    loop {
        if let Some(current) = pending.take() {
            match current {
                Value::Array(items) => {
                    emit("[");
                    stack.push(JsonFrame::Array { items, index: 0 });
                }
                Value::Object(map) => {
                    emit("{");
                    let entries: Vec<(&String, &Value)> = map.iter().collect();
                    stack.push(JsonFrame::Object { entries, index: 0 });
                }
                scalar => {
                    emit(&serde_json::to_string(scalar).unwrap_or_else(|_| "null".to_owned()));
                }
            }
        }
        let Some(top) = stack.last_mut() else {
            return;
        };
        match top {
            JsonFrame::Array { items, index } => {
                if *index == items.len() {
                    stack.pop();
                    emit("]");
                    continue;
                }
                if *index > 0 {
                    emit(",");
                }
                let child = &items[*index];
                *index += 1;
                pending = Some(child);
            }
            JsonFrame::Object { entries, index } => {
                if *index == entries.len() {
                    stack.pop();
                    emit("}");
                    continue;
                }
                if *index > 0 {
                    emit(",");
                }
                let (key, child) = entries[*index];
                *index += 1;
                emit(&serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()));
                emit(":");
                pending = Some(child);
            }
        }
    }
}

/// The text's size without building it: UTF-8 bytes (wire cost) and UTF-16 code units (stored cost).
pub fn json_size(value: &Value) -> (usize, usize) {
    let mut bytes = 0usize;
    let mut units = 0usize;
    serialize_piece_into(value, |piece| {
        bytes += piece.len();
        units += usize::try_from(utf16_len(piece)).unwrap_or(0);
    });
    (bytes, units)
}

/// Pieces regrouped into chunks of at most `max_units` UTF-16 code units without building the whole string.
pub fn json_chunks(value: &Value, max_units: usize) -> Vec<String> {
    let max_units = max_units.max(1);
    let mut chunks = Vec::new();
    let mut pending = String::new();
    let mut pending_units = 0usize;

    let mut flush_pending = |pending: &mut String, pending_units: &mut usize| {
        while *pending_units >= max_units {
            let cut_byte = crate::compact_content_store::utf16_boundary(pending, max_units);
            if cut_byte == 0 {
                break;
            }
            let head = pending[..cut_byte].to_owned();
            let rest = pending[cut_byte..].to_owned();
            *pending = rest;
            *pending_units = usize::try_from(utf16_len(pending)).unwrap_or(0);
            chunks.push(head);
        }
    };

    serialize_piece_into(value, |piece| {
        pending.push_str(piece);
        pending_units += usize::try_from(utf16_len(piece)).unwrap_or(0);
        if pending_units >= max_units {
            flush_pending(&mut pending, &mut pending_units);
        }
    });
    if !pending.is_empty() {
        chunks.push(pending);
    }
    chunks
}

/// Ordinary records and host snapshots share the bounded fragment/staging substrate.
pub fn stage_record(
    staging: &TransferStaging,
    record: &Value,
    content: RecordContent,
) -> Result<StagedContent, StagingOpError> {
    let kind = protocol_id("event");
    let id = protocol_id("record");
    match content {
        RecordContent::Text(text) => {
            let trimmed = text.strip_suffix('\n').unwrap_or(&text);
            staging.stage(kind, id, trimmed)
        }
        RecordContent::Units(units) => {
            let chunks = json_chunks(record, FRAGMENT_MAX_BYTES / 2);
            let mut transfer = staging.begin(kind, id, units * 2)?;
            for chunk in &chunks {
                transfer.append(chunk)?;
            }
            transfer.commit()
        }
        RecordContent::Chunks(chunks) => {
            let units: usize = chunks
                .iter()
                .map(|c| usize::try_from(utf16_len(c)).unwrap_or(0))
                .sum();
            let mut transfer = staging.begin(kind, id, units * 2)?;
            for chunk in &chunks {
                transfer.append(chunk)?;
            }
            transfer.commit()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compact_content_store::CompactContentStore;
    use crate::compact_content_store::CompactContentStoreOptions;
    use serde_json::json;

    #[test]
    fn json_chunks_and_size_match_serde_json() {
        let value = json!({"a": [1, true, "hello", {"nested": null}], "b": "world"});
        let expected = serde_json::to_string(&value).expect("serialize");
        let chunks = json_chunks(&value, 5);
        assert_eq!(chunks.concat(), expected);
        let (bytes, units) = json_size(&value);
        assert_eq!(bytes, expected.len());
        assert_eq!(units, expected.encode_utf16().count());
    }

    #[tokio::test]
    async fn stages_record_from_text_units_and_chunks() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = CompactContentStore::new(CompactContentStoreOptions {
            dir: dir.path().join("epoch").display().to_string(),
            ..Default::default()
        });
        let staging = TransferStaging::new(store, None);
        let record = json!({"type":"message_end","ok":true});
        let line = format!("{}\n", serde_json::to_string(&record).expect("json"));

        let s1 = stage_record(&staging, &record, RecordContent::Text(line)).expect("text");
        let (_, units) = json_size(&record);
        let s2 = stage_record(&staging, &record, RecordContent::Units(units)).expect("units");
        let s3 = stage_record(&staging, &record, RecordContent::Chunks(vec!["{}".into()]))
            .expect("chunks");
        assert_eq!(s1.ref_.entry().digest, s2.ref_.entry().digest);
        assert_eq!(s3.ref_.entry().length, 4);
        staging.release(&s1);
        staging.release(&s2);
        staging.release(&s3);
        staging.close().await.expect("close");
    }
}
