//! Port of `tools/output/transcript/session-dir.ts`.

use std::fs;
use std::io;
use std::path::Path;

use crate::tools::output::transcript::read_bounded::{MAX_TRANSCRIPT_SOURCE_BYTES, read_bounded_file_text};
use crate::tools::output::transcript::session_jsonl::parse_session_transcript;
use crate::tools::output::types::{TranscriptEntry, TranscriptReadResult, TranscriptSource};

/// Where the rpc child writes its own senpi session JSONL. Mirrors the launch layout: the manager
/// nests each child under children/<taskId>, and the rpc spawn isolates the session under
/// sessions/<taskId>. READ-ONLY: task_output never writes here and only ever touches OUR own state
/// dir, never another project's.
pub fn child_session_dir(state_dir: &str, task_id: &str) -> String {
    Path::new(state_dir)
        .join("children")
        .join(task_id)
        .join("sessions")
        .join(task_id)
        .to_string_lossy()
        .into_owned()
}

/// Reconstruct a child's transcript from its persisted senpi session file(s). Multiple files (one per
/// session turn) are read in name order and concatenated. A missing dir is an empty transcript.
pub fn read_session_dir_transcript(state_dir: &str, task_id: &str) -> io::Result<Vec<TranscriptEntry>> {
    Ok(read_session_dir_transcript_result(state_dir, task_id)?.entries)
}

pub fn read_session_dir_transcript_result(state_dir: &str, task_id: &str) -> io::Result<TranscriptReadResult> {
    let dir = child_session_dir(state_dir, task_id);
    let files = list_jsonl(Path::new(&dir))?;
    if files.is_empty() {
        return Ok(TranscriptReadResult {
            entries: Vec::new(),
            source: TranscriptSource::SessionJsonl,
            truncated: Some(false),
        });
    }
    let selected: Vec<&String> = if files.len() <= 2 {
        files.iter().collect()
    } else {
        files.first().into_iter().chain(files.last()).collect()
    };
    let file_budget = MAX_TRANSCRIPT_SOURCE_BYTES / selected.len();
    let mut entries = Vec::new();
    let mut truncated = selected.len() < files.len();
    for file in selected {
        if let Some(raw) = read_bounded_file_text(Path::new(&dir).join(file), Some(file_budget))? {
            entries.extend(parse_session_transcript(&raw.text));
            truncated = truncated || raw.truncated;
        }
    }
    Ok(TranscriptReadResult {
        entries,
        source: TranscriptSource::SessionJsonl,
        truncated: Some(truncated),
    })
}

fn list_jsonl(dir: &Path) -> io::Result<Vec<String>> {
    let read_dir = match fs::read_dir(dir) {
        Ok(read_dir) => read_dir,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut files = Vec::new();
    for entry in read_dir {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".jsonl") {
            files.push(name);
        }
    }
    files.sort();
    Ok(files)
}
