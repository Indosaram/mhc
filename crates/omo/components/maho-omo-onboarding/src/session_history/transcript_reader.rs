//! Streaming transcript reads with identity checks (upstream `transcript-reader.ts`).

use super::constants::{
    MAX_IMPORT_HISTORY_BYTES, MAX_IMPORT_RECORD_BYTES, MAX_IMPORTED_TRANSCRIPT_BYTES,
    TRANSCRIPT_PREFIX_BYTES,
};
use super::session_history_fs::{FileStats, SessionHistoryFile, SessionHistoryFs};
use super::transcript_records::{
    TranscriptRecord, decode_transcript_line, should_retain_decoded_record,
};
use super::types::{AgentSessionSource, CompletedTranscript};

/// A [`super::types::TranscriptSource`] without `provider`, `homePath` and `providerSessionId`.
#[derive(Clone, Debug, PartialEq)]
pub struct TranscriptIdentity {
    pub file_path: String,
    pub size: u64,
    pub mtime_ms: Option<i64>,
    pub device: u64,
    pub inode: Option<u64>,
    pub birthtime_ms: Option<i64>,
}

pub fn transcript_identity(file_path: &str, stats: &FileStats) -> TranscriptIdentity {
    TranscriptIdentity {
        file_path: file_path.to_owned(),
        size: stats.size,
        mtime_ms: stats.mtime_ms,
        device: stats.dev,
        inode: stats.ino,
        birthtime_ms: stats.birthtime_ms,
    }
}

pub fn same_transcript_identity(left: &TranscriptIdentity, right: &TranscriptIdentity) -> bool {
    left == right
}

impl CompletedTranscript {
    pub fn identity(&self) -> TranscriptIdentity {
        TranscriptIdentity {
            file_path: self.file_path.clone(),
            size: self.size,
            mtime_ms: self.mtime_ms,
            device: self.device,
            inode: self.inode,
            birthtime_ms: self.birthtime_ms,
        }
    }
}

struct HistoryBudgetExceeded;

struct RecordAccumulator {
    source: AgentSessionSource,
    record_limit: usize,
    records: Vec<TranscriptRecord>,
    record_count: usize,
    chunks: Vec<u8>,
    pending_bytes: usize,
    oversized: bool,
    history_bytes: usize,
}

impl RecordAccumulator {
    fn new(source: AgentSessionSource, record_limit: usize) -> Self {
        Self {
            source,
            record_limit,
            records: Vec::new(),
            record_count: 0,
            chunks: Vec::new(),
            pending_bytes: 0,
            oversized: false,
            history_bytes: 0,
        }
    }

    fn started(&self) -> bool {
        self.pending_bytes > 0 || self.oversized
    }

    fn append(&mut self, segment: &[u8]) {
        if self.oversized || segment.is_empty() {
            return;
        }
        self.pending_bytes += segment.len();
        if self.pending_bytes > MAX_IMPORT_RECORD_BYTES {
            // A record past the cap is skipped whole, never buffered.
            self.oversized = true;
            self.chunks = Vec::new();
            return;
        }
        self.chunks.extend_from_slice(segment);
    }

    /// `Ok(false)` once the transcript holds more records than the caller's remaining budget.
    fn finish(&mut self) -> Result<bool, HistoryBudgetExceeded> {
        self.record_count += 1;
        if self.record_count > self.record_limit {
            return Ok(false);
        }
        if !self.oversized && self.pending_bytes > 0 {
            let line = String::from_utf8_lossy(&self.chunks).into_owned();
            if let Some(decoded) = decode_transcript_line(&line, self.source)
                && should_retain_decoded_record(self.source, &decoded)
            {
                self.history_bytes += decoded.to_json().to_string().encode_utf16().count() * 2;
                if self.history_bytes > MAX_IMPORT_HISTORY_BYTES {
                    return Err(HistoryBudgetExceeded);
                }
                self.records.push(decoded);
            }
        }
        self.chunks.clear();
        self.pending_bytes = 0;
        self.oversized = false;
        Ok(true)
    }
}

fn read_records(
    file: &mut dyn SessionHistoryFile,
    expected: &TranscriptIdentity,
    accumulator: &mut RecordAccumulator,
) -> Result<bool, HistoryBudgetExceeded> {
    let mut bytes_read: u64 = 0;
    while bytes_read < expected.size {
        let want =
            usize::try_from((expected.size - bytes_read).min(TRANSCRIPT_PREFIX_BYTES as u64))
                .unwrap_or(TRANSCRIPT_PREFIX_BYTES);
        let Ok(Some(next)) = file.read(want) else {
            return Ok(false);
        };
        bytes_read += next.len() as u64;
        let mut start = 0;
        while start < next.len() {
            let newline = next[start..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|at| at + start);
            let end = newline.unwrap_or(next.len());
            accumulator.append(&next[start..end]);
            let Some(newline) = newline else { break };
            if !accumulator.finish()? {
                return Ok(false);
            }
            start = newline + 1;
        }
    }
    if accumulator.started() {
        accumulator.finish()
    } else {
        Ok(true)
    }
}

pub struct TranscriptSnapshot {
    pub records: Vec<TranscriptRecord>,
    pub record_count: usize,
}

/// Stream one transcript into its retained records. The file identity is checked on both sides of the
/// read, and any budget failure rejects the whole transcript before anything is imported.
pub fn read_transcript_records(
    fs: &dyn SessionHistoryFs,
    expected: &TranscriptIdentity,
    record_limit: usize,
    source: AgentSessionSource,
) -> Option<TranscriptSnapshot> {
    if expected.size > MAX_IMPORTED_TRANSCRIPT_BYTES {
        return None;
    }
    let mut file = fs.open(&expected.file_path).ok()?;
    let result = (|| {
        let before = file.stat().ok()?;
        if !same_transcript_identity(expected, &transcript_identity(&expected.file_path, &before)) {
            return None;
        }
        let mut accumulator = RecordAccumulator::new(source, record_limit);
        if !read_records(file.as_mut(), expected, &mut accumulator).ok()? {
            return None;
        }
        let after = file.stat().ok()?;
        if !same_transcript_identity(expected, &transcript_identity(&expected.file_path, &after)) {
            return None;
        }
        Some(TranscriptSnapshot {
            records: accumulator.records,
            record_count: accumulator.record_count,
        })
    })();
    let _ = file.close();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::session_history_fs::{NodeSessionHistoryFs, stat_or_null};

    fn read(
        contents: &str,
        limit: usize,
    ) -> Result<Option<TranscriptSnapshot>, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("t.jsonl").to_string_lossy().into_owned();
        std::fs::write(&path, contents)?;
        let stats = stat_or_null(&NodeSessionHistoryFs, &path).ok_or("stat")?;
        Ok(read_transcript_records(
            &NodeSessionHistoryFs,
            &transcript_identity(&path, &stats),
            limit,
            AgentSessionSource::ClaudeAgent,
        ))
    }

    #[test]
    fn reads_retained_records_and_counts_every_line() -> Result<(), Box<dyn std::error::Error>> {
        let snapshot = read(
            "{\"type\":\"user\",\"cwd\":\"/p\"}\n{\"type\":\"noise\"}\nnot json\n",
            10,
        )?
        .ok_or("none")?;
        assert_eq!(snapshot.record_count, 3);
        assert_eq!(snapshot.records.len(), 1);
        Ok(())
    }

    #[test]
    fn record_over_the_budget_rejects_the_transcript() -> Result<(), Box<dyn std::error::Error>> {
        assert!(read("{}\n{}\n{}\n", 2)?.is_none());
        assert!(read("{}\n{}", 2)?.is_some());
        Ok(())
    }

    #[test]
    fn replaced_file_identity_is_rejected() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("t.jsonl").to_string_lossy().into_owned();
        std::fs::write(&path, "{}\n")?;
        let stats = stat_or_null(&NodeSessionHistoryFs, &path).ok_or("stat")?;
        let mut identity = transcript_identity(&path, &stats);
        identity.size += 1;
        assert!(
            read_transcript_records(
                &NodeSessionHistoryFs,
                &identity,
                10,
                AgentSessionSource::Codex
            )
            .is_none()
        );
        Ok(())
    }
}
