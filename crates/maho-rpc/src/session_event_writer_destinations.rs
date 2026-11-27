//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-destinations.ts`.
//!
//! Lazy per-destination JSONL encoding and compact lease/materializer projection helpers.

use crate::json_pieces::json_size;
use crate::jsonl::serialize_json_line;
use crate::media_placeholders::omit_inline_media;
use serde_json::Value;

/// Maximum serialized size of a single compact record before replacement with a lease/pointer.
pub const COMPACT_RECORD_MAX_BYTES: usize = 64 * 1024;

/// A record's legacy line, encoded on first use so a record no legacy destination needs never
/// builds it (RULING-81 X06).
pub struct RecordLine {
    record: Value,
    encoded: Option<String>,
}

impl RecordLine {
    pub fn new(record: Value, encoded: Option<String>) -> Self {
        Self { record, encoded }
    }

    pub fn text(&mut self) -> Result<&str, serde_json::Error> {
        if self.encoded.is_none() {
            self.encoded = Some(serialize_json_line(&self.record)?);
        }
        Ok(self.encoded.as_deref().unwrap_or_default())
    }

    pub fn built(&self) -> Option<&str> {
        self.encoded.as_deref()
    }

    pub fn record(&self) -> &Value {
        &self.record
    }
}

/// Checks whether a serialized record fits within the compact record byte budget.
pub fn fits_compact_record_budget(record: &Value) -> bool {
    json_size(record).bytes <= COMPACT_RECORD_MAX_BYTES
}

/// Builds a media-omitted projection line for peers with `media_placeholders`.
pub fn omitted_media_line(record: &Value) -> Result<String, serde_json::Error> {
    let omitted = omit_inline_media(record);
    serialize_json_line(&omitted)
}
