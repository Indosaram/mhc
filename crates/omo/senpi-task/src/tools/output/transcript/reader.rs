//! Port of `tools/output/transcript/reader.ts`.

use std::io;
use std::sync::Arc;

use crate::tools::output::transcript::event_log::read_event_log_transcript_result;
use crate::tools::output::transcript::session_dir::read_session_dir_transcript_result;
use crate::tools::output::types::{TranscriptReadResult, TranscriptReader, TranscriptReaderInput, TranscriptSource};

/// The default transcript source resolution: OUR in-process event log first (authoritative for
/// in-process children we instrumented), falling back to the rpc child's own persisted session JSONL.
/// Reports which source answered so task_output can surface it. Never fails on absent state.
pub fn default_transcript_reader(input: &TranscriptReaderInput<'_>) -> io::Result<TranscriptReadResult> {
    let event_log = read_event_log_transcript_result(input.state_dir, input.task_id)?;
    if !event_log.entries.is_empty() || event_log.truncated == Some(true) {
        return Ok(event_log);
    }

    let session = read_session_dir_transcript_result(input.state_dir, input.task_id)?;
    if !session.entries.is_empty() || session.truncated == Some(true) {
        return Ok(session);
    }

    Ok(TranscriptReadResult {
        entries: Vec::new(),
        source: TranscriptSource::None,
        truncated: None,
    })
}

/// `defaultTranscriptReader` as a shareable `TranscriptReader` value.
pub fn default_transcript_reader_fn() -> TranscriptReader {
    Arc::new(default_transcript_reader)
}
