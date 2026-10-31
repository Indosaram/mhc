//! Answer an `effectId` from a transcript on disk (senpi `rpc/effect-transcript-lookup.ts`), reading
//! custom entries from the tail backwards until the id is found or the header is reached. `found: false`
//! is answered ONLY after the whole file was searched; a file that cannot be read is
//! [`TranscriptUnreadable`], because a caller treats "not found" as proof the effect never landed.
use crate::client_admission_record::{CLIENT_ADMISSION_ENTRY, parse_client_admission_record};
use crate::effect_record::{EffectStatus, RPC_EFFECT_ENTRY, parse_rpc_effect_record};
use crate::effect_status::{project_admission, project_effect};
use serde_json::Value;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The first backwards pass; later chunks continue until the header, so it is never a cut-off.
pub const EFFECT_LOOKUP_CHUNK_BYTES: u64 = 4 * 1024 * 1024;
const HEADER_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EffectLookup {
    Found(EffectStatus),
    /// Searched to the header: the effect is not in this transcript.
    NotFoundComplete,
}

#[derive(Debug, thiserror::Error)]
#[error("{reason}")]
pub struct TranscriptUnreadable {
    pub path: PathBuf,
    pub reason: String,
}
impl TranscriptUnreadable {
    pub const CODE: &'static str = "transcript_unreadable";
    pub const ACTION: &'static str = "check the sessionPath, or query the loaded session by sessionId; do not treat this as not found";
    fn new(path: &Path, cause: impl std::fmt::Display) -> Self {
        Self {
            path: path.to_path_buf(),
            reason: format!("transcript_unreadable: {cause}"),
        }
    }
}

pub fn lookup_effect_in_transcript(
    path: &Path,
    effect_id: &str,
) -> Result<EffectLookup, TranscriptUnreadable> {
    lookup_with_chunk(path, effect_id, EFFECT_LOOKUP_CHUNK_BYTES)
}

fn lookup_with_chunk(
    path: &Path,
    effect_id: &str,
    chunk_bytes: u64,
) -> Result<EffectLookup, TranscriptUnreadable> {
    let unreadable = |cause: std::io::Error| TranscriptUnreadable::new(path, cause);
    let mut file = std::fs::File::open(path).map_err(unreadable)?;
    let size = file.metadata().map_err(unreadable)?.len();
    let durable_session_id = read_header_id(&mut file, path, size)?;
    let mut end = size;
    let mut carry: Vec<u8> = Vec::new();
    while end > 0 {
        let start = end.saturating_sub(chunk_bytes);
        let mut chunk = vec![0u8; (end - start) as usize];
        file.seek(SeekFrom::Start(start)).map_err(unreadable)?;
        file.read_exact(&mut chunk).map_err(unreadable)?;
        chunk.extend_from_slice(&carry);
        let data = chunk;
        end = start;
        // Every chunk but the first in the file starts mid-line: keep that head for the next chunk.
        let newline = data.iter().position(|byte| *byte == b'\n');
        let first_newline = if start == 0 {
            None
        } else if let Some(index) = newline {
            Some(index)
        } else {
            carry = data;
            continue;
        };
        let body_start = first_newline.map_or(0, |index| index + 1);
        carry = first_newline.map_or_else(Vec::new, |index| data[..index].to_vec());
        let text = String::from_utf8_lossy(&data[body_start..]);
        if let Some(effect) = newest_in_lines(&text, effect_id, &durable_session_id) {
            return Ok(EffectLookup::Found(effect));
        }
    }
    Ok(EffectLookup::NotFoundComplete)
}

fn read_header_id(
    file: &mut std::fs::File,
    path: &Path,
    size: u64,
) -> Result<String, TranscriptUnreadable> {
    let mut head = vec![0u8; size.min(HEADER_BYTES) as usize];
    file.seek(SeekFrom::Start(0))
        .and_then(|_| file.read_exact(&mut head))
        .map_err(|cause| TranscriptUnreadable::new(path, cause))?;
    let line_end = head
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(head.len());
    let header: Option<Value> = serde_json::from_slice(&head[..line_end]).ok();
    header
        .filter(|header| header.get("type").and_then(Value::as_str) == Some("session"))
        .and_then(|header| header.get("id").and_then(Value::as_str).map(str::to_owned))
        .ok_or_else(|| TranscriptUnreadable::new(path, "the file has no session header"))
}

fn newest_in_lines(text: &str, effect_id: &str, durable_session_id: &str) -> Option<EffectStatus> {
    let encoded = serde_json::to_string(effect_id).ok()?;
    let encoded_id = &encoded[1..encoded.len() - 1];
    for line in text.split('\n').rev() {
        if !line.contains(encoded_id) {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("custom") {
            continue;
        }
        let (Some(custom_type), Some(data)) = (
            entry.get("customType").and_then(Value::as_str),
            entry.get("data"),
        ) else {
            continue;
        };
        if custom_type == RPC_EFFECT_ENTRY {
            if let Some(record) = parse_rpc_effect_record(data)
                && record.effect_id == effect_id
                && record.durable_session_id == durable_session_id
            {
                return Some(project_effect(&record, None));
            }
        } else if custom_type == CLIENT_ADMISSION_ENTRY
            && let Some(record) = parse_client_admission_record(data)
            && record.client_message_id == effect_id
            && record.durable_session_id == durable_session_id
        {
            return Some(project_admission(&record, None));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect_record::EffectState;
    use serde_json::json;

    fn effect(id: &str, state: &str, durable: &str) -> String {
        json!({"type":"custom","customType":"rpc_effect","data":{"effectId":id,"kind":"abort","state":state,"by":{"connectionId":"c"},"acceptedAt":"a","updatedAt":"u","durableSessionId":durable,"fingerprint":"f"}}).to_string()
    }

    fn transcript(lines: &[String]) -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("temp");
        let mut text = json!({"type":"session","id":"d1"}).to_string();
        for line in lines {
            text.push('\n');
            text.push_str(line);
        }
        text.push('\n');
        std::fs::write(file.path(), text).expect("write");
        file
    }

    #[test]
    fn the_newest_record_wins_across_small_chunks() {
        let padding = json!({"type":"message","text":"x".repeat(200)}).to_string();
        let file = transcript(&[
            effect("e1", "accepted", "d1"),
            padding.clone(),
            effect("e1", "done", "d1"),
            padding,
            effect("e1", "failed", "other"),
        ]);
        match lookup_with_chunk(file.path(), "e1", 64).expect("read") {
            EffectLookup::Found(status) => assert_eq!(status.state, EffectState::Done),
            EffectLookup::NotFoundComplete => panic!("not found"),
        }
        assert_eq!(
            lookup_with_chunk(file.path(), "e2", 64).expect("read"),
            EffectLookup::NotFoundComplete
        );
    }

    #[test]
    fn admissions_answer_by_client_message_id() {
        let admission = json!({"type":"custom","customType":"rpc_client_message_admission","data":{"clientMessageId":"c1","durableSessionId":"d1","fingerprint":"f","disposition":"started","state":"running"}}).to_string();
        let file = transcript(&[admission]);
        assert!(
            matches!(lookup_effect_in_transcript(file.path(), "c1"), Ok(EffectLookup::Found(status)) if status.state == EffectState::Applied)
        );
    }

    #[test]
    fn missing_or_headerless_files_are_unreadable_not_absent() {
        let error = lookup_effect_in_transcript(Path::new("/nonexistent/x.jsonl"), "e")
            .expect_err("missing");
        assert!(error.reason.starts_with("transcript_unreadable: "));
        let file = tempfile::NamedTempFile::new().expect("temp");
        std::fs::write(file.path(), "{\"type\":\"message\"}\n").expect("write");
        let error = lookup_effect_in_transcript(file.path(), "e").expect_err("headerless");
        assert_eq!(
            error.reason,
            "transcript_unreadable: the file has no session header"
        );
    }
}
