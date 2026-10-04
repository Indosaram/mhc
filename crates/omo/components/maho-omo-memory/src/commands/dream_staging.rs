//! External transcript staging for `/dream --from transcript:<path>`.
//! Port of `components/memory/commands/dream-staging.ts` at pin 77f3067f1.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use memory_core::journal::{
    entries::{TranscriptEntry, project_transcript_entries},
    store::{TranscriptJournal, TranscriptJournalOptions},
};
use serde_json::Value;

use crate::journal_wiring::project_session_entries;

const MAX_TRANSCRIPT_BYTES: u64 = 64 * 1024 * 1024;

pub struct StagedDreamTranscript {
    pub conversation_id: String,
    pub staged_message_ids: Vec<String>,
    pub skipped_message_ids: Vec<String>,
}

struct NormalizedSessionEntry {
    value: Value,
    captured_at: String,
}

pub fn stage_dream_transcript(
    input_path: &str,
    transcripts_dir: &Path,
    cwd: &Path,
) -> Result<StagedDreamTranscript, String> {
    let file_path = resolve_path(cwd, input_path);
    let metadata = std::fs::metadata(&file_path).map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("--from transcript:<path> must name a senpi session JSONL file".to_owned());
    }
    if metadata.len() > MAX_TRANSCRIPT_BYTES {
        return Err("--from transcript file exceeds the 64 MiB limit".to_owned());
    }

    let bytes = std::fs::read(&file_path).map_err(|error| error.to_string())?;
    let raw = String::from_utf8(bytes)
        .map_err(|_| "--from transcript file must be valid UTF-8".to_owned())?;

    let fallback_captured_at = file_mtime_iso(&metadata);
    let mut normalized: Vec<NormalizedSessionEntry> = Vec::new();
    let mut has_session_header = false;
    for (line_index, line) in raw.split('\n').enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let Ok(parsed) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if !parsed.is_object() {
            continue;
        }
        if parsed.get("type").and_then(Value::as_str) == Some("session")
            && parsed.get("id").and_then(Value::as_str).is_some()
        {
            has_session_header = true;
            continue;
        }
        if let Some(item) = normalize_message(&parsed, &file_path, line_index, &fallback_captured_at)
        {
            normalized.push(item);
        }
    }
    if !has_session_header || normalized.is_empty() {
        return Err(
            "--from accepts only senpi session JSONL with a session header and message rows"
                .to_owned(),
        );
    }

    let mut captured_at_by_id: BTreeMap<String, String> = BTreeMap::new();
    for item in &normalized {
        if let Some(id) = item.value.get("id").and_then(Value::as_str) {
            captured_at_by_id.insert(id.to_owned(), item.captured_at.clone());
        }
    }
    let values: Vec<Value> = normalized.into_iter().map(|item| item.value).collect();
    let projections = project_session_entries(&values);
    let mut entries: Vec<TranscriptEntry> = Vec::new();
    for projection in &projections {
        let message_id = projection_message_id(projection);
        let captured_at = captured_at_by_id
            .get(&message_id)
            .cloned()
            .unwrap_or_else(|| fallback_captured_at.clone());
        entries.extend(project_transcript_entries(projection, &captured_at));
    }
    if entries.is_empty() {
        return Err(
            "--from senpi session JSONL contains no stageable user or assistant messages".to_owned(),
        );
    }

    let conversation_id = format!(
        "from-transcript-{}",
        &sha1_hex(file_path.to_string_lossy().as_bytes())[..12]
    );
    let journal = TranscriptJournal::new(TranscriptJournalOptions::new(
        transcripts_dir.join(&conversation_id),
    ));
    let existing_message_ids: BTreeSet<String> = journal
        .read_entries()
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.source_message_id().to_owned())
        .collect();
    let fresh: Vec<TranscriptEntry> = entries
        .iter()
        .filter(|entry| !existing_message_ids.contains(entry.source_message_id()))
        .cloned()
        .collect();
    let staged_message_ids = unique(
        fresh
            .iter()
            .map(|entry| entry.source_message_id().to_owned())
            .collect(),
    );
    let skipped_message_ids = unique(
        entries
            .iter()
            .filter(|entry| existing_message_ids.contains(entry.source_message_id()))
            .map(|entry| entry.source_message_id().to_owned())
            .collect(),
    );
    journal.append(&fresh).map_err(|error| error.to_string())?;
    Ok(StagedDreamTranscript { conversation_id, staged_message_ids, skipped_message_ids })
}

fn projection_message_id(projection: &memory_core::journal::entries::TranscriptProjection) -> String {
    use memory_core::journal::entries::TranscriptProjection;
    match projection {
        TranscriptProjection::User { message_id, .. }
        | TranscriptProjection::Error { message_id, .. }
        | TranscriptProjection::Assistant { message_id, .. } => message_id.clone(),
    }
}

fn normalize_message(
    row: &Value,
    file_path: &Path,
    line_index: usize,
    fallback_captured_at: &str,
) -> Option<NormalizedSessionEntry> {
    if row.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = row.get("message")?;
    if !message.is_object() {
        return None;
    }
    if message.get("role").and_then(Value::as_str).is_none() {
        return None;
    }
    let supplied_id = row.get("source_message_id").and_then(Value::as_str);
    let text = message_text(message);
    let id = supplied_id.map(str::to_owned).unwrap_or_else(|| {
        let seed = format!("{}:{line_index}:{text}", file_path.display());
        sha1_hex(seed.as_bytes())[..16].to_owned()
    });
    let captured_at = row
        .get("captured_at")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| fallback_captured_at.to_owned());
    let mut value = row.clone();
    if let Some(object) = value.as_object_mut() {
        object.insert("id".to_owned(), Value::String(id));
    }
    Some(NormalizedSessionEntry { value, captured_at })
}

fn message_text(message: &Value) -> String {
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        return text.to_owned();
    }
    let Some(content) = message.get("content").and_then(Value::as_array) else {
        return String::new();
    };
    let mut values: Vec<String> = Vec::new();
    for part in content {
        let Some(part) = part.as_object() else {
            continue;
        };
        for key in ["text", "thinking"] {
            if let Some(value) = part.get(key).and_then(Value::as_str) {
                values.push(value.to_owned());
            }
        }
        if part.get("type").and_then(Value::as_str) == Some("toolCall")
            && let Some(arguments) = part.get("arguments")
        {
            values.push(safe_stringify(arguments));
        }
    }
    values.join("\n")
}

fn safe_stringify(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_owned();
    }
    serde_json::to_string(value).unwrap_or_default()
}

fn unique(values: Vec<String>) -> Vec<String> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for value in values {
        if seen.insert(value.clone()) {
            out.push(value);
        }
    }
    out
}

fn resolve_path(cwd: &Path, input_path: &str) -> PathBuf {
    let path = Path::new(input_path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    }
}

fn file_mtime_iso(metadata: &std::fs::Metadata) -> String {
    metadata
        .modified()
        .ok()
        .map(chrono::DateTime::<chrono::Utc>::from)
        .map(|instant| instant.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default()
}

/// Minimal SHA-1 (FIPS 180-4), matching `node:crypto` sha1 digests byte for byte.
pub fn sha1_hex(input: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let bit_len = (input.len() as u64) * 8;
    let mut message = input.to_vec();
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (index, word) in chunk.chunks_exact(4).enumerate() {
            w[index] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for index in 16..80 {
            w[index] = (w[index - 3] ^ w[index - 8] ^ w[index - 14] ^ w[index - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (index, word) in w.iter().enumerate() {
            let (f, k) = match index {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let temp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = temp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_known_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    }
}
