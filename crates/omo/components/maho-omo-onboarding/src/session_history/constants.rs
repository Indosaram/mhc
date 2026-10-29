//! Bounds shared by every session-history reader (upstream `session-history/constants.ts`).

/// Chunk size for full transcript reads.
pub const TRANSCRIPT_PREFIX_BYTES: usize = 32 * 1024;
/// Small reads avoid wasting the metadata budget on long Codex instruction headers.
pub const METADATA_READ_BYTES: usize = 8 * 1024;
/// Prevent malformed transcripts from turning project discovery into a full file scan.
pub const MAX_TRANSCRIPT_SCAN_BYTES: usize = 1024 * 1024;

/// Upper bound on transcripts inspected per agent source.
pub const MAX_TRANSCRIPTS_PER_SOURCE: usize = 5000;
/// Directory reads and candidate stats share one budget per agent source.
pub const MAX_DISCOVERY_OPERATIONS_PER_SOURCE: i64 = (MAX_TRANSCRIPTS_PER_SOURCE as i64) * 4;
pub const MAX_METADATA_BYTES_PER_SOURCE: u64 = 64 * 1024 * 1024;
pub const MAX_METADATA_OPERATIONS_PER_SOURCE: i64 = (MAX_TRANSCRIPTS_PER_SOURCE as i64) * 4;
pub const MAX_METADATA_RECORDS_PER_SOURCE: usize = 100_000;
pub const MAX_METADATA_RECORDS_PER_TRANSCRIPT: usize = 1_000;
pub const DEFAULT_RECENT_WINDOW_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub const MAX_IMPORTED_TRANSCRIPT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_IMPORTED_MESSAGES: usize = 200;
pub const MAX_IMPORT_HISTORY_BYTES: usize = 32 * 1024 * 1024;
pub const MAX_IMPORT_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_IMPORT_TRANSCRIPTS: usize = 100;
pub const MAX_IMPORT_RECORDS: usize = 100_000;
/// A single JSONL record past this size is skipped whole instead of being buffered.
pub const MAX_IMPORT_RECORD_BYTES: usize = 4 * 1024 * 1024;

pub const MAX_OMO_DISCOVERY_OPERATIONS: i64 = 20_000;
pub const MAX_OMO_SESSION_HEADERS: usize = 5_000;
pub const OMO_HEADER_READ_BYTES: usize = 8192;
/// A session holding only startup records measures 1.4-2 KB; past this size it holds a conversation.
pub const OMO_STARTUP_ONLY_MAX_BYTES: u64 = 256 * 1024;

/// Digest bound: one digest per session stays in the 3-4 KB range.
pub const MAX_SESSION_DIGEST_BYTES: usize = 4096;

pub const PROJECT_SCAN_CAP: usize = MAX_TRANSCRIPTS_PER_SOURCE * 2 + MAX_OMO_SESSION_HEADERS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_caps_match_upstream() {
        assert_eq!(MAX_DISCOVERY_OPERATIONS_PER_SOURCE, 20_000);
        assert_eq!(MAX_METADATA_OPERATIONS_PER_SOURCE, 20_000);
        assert_eq!(PROJECT_SCAN_CAP, 15_000);
        assert_eq!(DEFAULT_RECENT_WINDOW_MS, 2_592_000_000);
    }
}
