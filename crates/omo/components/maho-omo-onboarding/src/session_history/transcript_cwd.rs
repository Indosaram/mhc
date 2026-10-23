//! Read a transcript's cwd within metadata budgets (upstream `transcript-cwd.ts`).

use super::constants::{
    MAX_METADATA_RECORDS_PER_TRANSCRIPT, MAX_TRANSCRIPT_SCAN_BYTES, METADATA_READ_BYTES,
};
use super::session_history_fs::{SessionHistoryFile, SessionHistoryFs};
use super::transcript_discovery::TranscriptCandidate;
use super::transcript_records::extract_cwd_from_line;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MetadataReadBudget {
    pub bytes_remaining: u64,
    pub operations_remaining: i64,
    pub records_remaining: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CwdTranscript {
    pub file_path: String,
    pub mtime_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CwdGroup {
    pub cwd: String,
    pub home_path: String,
    pub last_active_at_ms: i64,
    pub transcripts: Vec<CwdTranscript>,
}

fn reserve_record(budget: &mut MetadataReadBudget, records_read: &mut usize) -> bool {
    if *records_read == MAX_METADATA_RECORDS_PER_TRANSCRIPT || budget.records_remaining == 0 {
        budget.truncated = true;
        return false;
    }
    *records_read += 1;
    budget.records_remaining -= 1;
    true
}

fn scan_for_cwd(
    file: &mut dyn SessionHistoryFile,
    size: u64,
    budget: &mut MetadataReadBudget,
) -> Option<String> {
    let mut remaining: Vec<u8> = Vec::new();
    let mut bytes_read: u64 = 0;
    let mut records_read = 0_usize;
    let max_bytes = size.min(MAX_TRANSCRIPT_SCAN_BYTES as u64);
    let last_record =
        |remaining: &[u8], budget: &mut MetadataReadBudget, records_read: &mut usize| {
            let record = String::from_utf8_lossy(remaining).into_owned();
            if record.is_empty() || !reserve_record(budget, records_read) {
                None
            } else {
                extract_cwd_from_line(record.trim())
            }
        };
    while bytes_read < max_bytes {
        if budget.bytes_remaining == 0 || budget.operations_remaining == 0 {
            budget.truncated = true;
            return None;
        }
        let read_size = (max_bytes - bytes_read)
            .min(METADATA_READ_BYTES as u64)
            .min(budget.bytes_remaining);
        budget.operations_remaining -= 1;
        budget.bytes_remaining -= read_size;
        let Ok(next) = file.read(usize::try_from(read_size).unwrap_or(METADATA_READ_BYTES)) else {
            return None;
        };
        let Some(next) = next else {
            return last_record(&remaining, budget, &mut records_read);
        };
        bytes_read += next.len() as u64;
        remaining.extend_from_slice(&next);
        while let Some(newline) = remaining.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = remaining.drain(..=newline).collect();
            if !reserve_record(budget, &mut records_read) {
                return None;
            }
            let text = String::from_utf8_lossy(&line[..line.len() - 1]).into_owned();
            if let Some(cwd) = extract_cwd_from_line(text.trim()) {
                return Some(cwd);
            }
        }
    }
    if bytes_read < size {
        budget.truncated = true;
        return None;
    }
    last_record(&remaining, budget, &mut records_read)
}

/// A large history snapshot can precede session metadata, so bounded chunks are read until a record names its cwd.
pub fn read_transcript_cwd(
    fs: &dyn SessionHistoryFs,
    transcript: &TranscriptCandidate,
    budget: &mut MetadataReadBudget,
) -> Option<String> {
    if transcript.size == 0 {
        return None;
    }
    if budget.bytes_remaining == 0
        || budget.operations_remaining < 2
        || budget.records_remaining == 0
    {
        budget.truncated = true;
        return None;
    }
    budget.operations_remaining -= 1;
    let mut file = fs.open(&transcript.file_path).ok()?;
    let cwd = scan_for_cwd(file.as_mut(), transcript.size, budget);
    let _ = file.close();
    cwd
}

pub fn group_transcripts_by_cwd(
    fs: &dyn SessionHistoryFs,
    transcripts: &[TranscriptCandidate],
    budget: &mut MetadataReadBudget,
) -> Vec<CwdGroup> {
    let mut groups: Vec<CwdGroup> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for transcript in transcripts {
        let Some(cwd) = read_transcript_cwd(fs, transcript, budget) else {
            continue;
        };
        let key = format!("{}\0{}", transcript.home_path, cwd);
        let entry = CwdTranscript {
            file_path: transcript.file_path.clone(),
            mtime_ms: transcript.mtime_ms,
        };
        match index.get(&key) {
            Some(at) => {
                let group = &mut groups[*at];
                group.last_active_at_ms = group.last_active_at_ms.max(transcript.mtime_ms);
                group.transcripts.push(entry);
            }
            None => {
                index.insert(key, groups.len());
                groups.push(CwdGroup {
                    cwd,
                    home_path: transcript.home_path.clone(),
                    last_active_at_ms: transcript.mtime_ms,
                    transcripts: vec![entry],
                });
            }
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::constants::{
        MAX_METADATA_BYTES_PER_SOURCE, MAX_METADATA_OPERATIONS_PER_SOURCE,
        MAX_METADATA_RECORDS_PER_SOURCE,
    };
    use crate::session_history::session_history_fs::NodeSessionHistoryFs;

    fn budget() -> MetadataReadBudget {
        MetadataReadBudget {
            bytes_remaining: MAX_METADATA_BYTES_PER_SOURCE,
            operations_remaining: MAX_METADATA_OPERATIONS_PER_SOURCE,
            records_remaining: MAX_METADATA_RECORDS_PER_SOURCE,
            truncated: false,
        }
    }

    fn candidate(
        dir: &std::path::Path,
        name: &str,
        body: &str,
        mtime: i64,
    ) -> Result<TranscriptCandidate, std::io::Error> {
        let path = dir.join(name);
        std::fs::write(&path, body)?;
        Ok(TranscriptCandidate {
            file_path: path.to_string_lossy().into_owned(),
            mtime_ms: mtime,
            home_path: "h".into(),
            size: body.len() as u64,
        })
    }

    #[test]
    fn finds_cwd_after_leading_records_and_groups_by_cwd() -> Result<(), Box<dyn std::error::Error>>
    {
        let dir = tempfile::tempdir()?;
        let a = candidate(dir.path(), "a.jsonl", "{\"x\":1}\n{\"cwd\":\"/p\"}\n", 5)?;
        let b = candidate(dir.path(), "b.jsonl", "{\"payload\":{\"cwd\":\"/p\"}}", 9)?;
        let empty = candidate(dir.path(), "c.jsonl", "", 1)?;
        let groups = group_transcripts_by_cwd(&NodeSessionHistoryFs, &[a, b, empty], &mut budget());
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].cwd, "/p");
        assert_eq!(groups[0].last_active_at_ms, 9);
        assert_eq!(groups[0].transcripts.len(), 2);
        Ok(())
    }

    #[test]
    fn starved_budget_marks_truncated() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let a = candidate(dir.path(), "a.jsonl", "{\"cwd\":\"/p\"}\n", 5)?;
        let mut starved = budget();
        starved.operations_remaining = 1;
        assert!(read_transcript_cwd(&NodeSessionHistoryFs, &a, &mut starved).is_none());
        assert!(starved.truncated);
        Ok(())
    }
}
