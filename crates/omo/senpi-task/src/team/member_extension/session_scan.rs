use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;

use serde_json::Value;

/// Detects whether a delivered peer_message envelope has landed in the member's session JSONL.
/// This is the delivery ack the self-poller uses before committing a reservation.
pub fn session_jsonl_contains_message(session_dir: &Path, message_id: &str) -> io::Result<bool> {
    let read_dir = match fs::read_dir(session_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if is_missing_path(&error) => return Ok(false),
        Err(error) => return Err(error),
    };
    let mut entries: Vec<String> = Vec::new();
    for entry in read_dir {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if is_missing_path(&error) => return Ok(false),
            Err(error) => return Err(error),
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".jsonl") {
            entries.push(name);
        }
    }
    entries.sort();

    let needle = format!("messageId=\"{message_id}\"");
    let mut buf: Vec<u8> = Vec::new();
    for entry in &entries {
        let mut reader = BufReader::new(fs::File::open(session_dir.join(entry))?);
        loop {
            buf.clear();
            if reader.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            let raw = buf.strip_suffix(b"\n").unwrap_or(buf.as_slice());
            let line = String::from_utf8_lossy(raw);
            // The marker embeds the id verbatim, so lines without it can never match.
            if !line.contains(message_id) {
                continue;
            }
            if let Some(value) = parse_json_line(&line)
                && contains_envelope_marker(&value, &needle)
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Sorted `(name, len, mtime)` of every `*.jsonl` in the session dir. Transcripts are
/// append-only, so an unchanged signature means a previous miss is still a miss.
pub type SessionJsonlSignature = Vec<(String, u64, Option<SystemTime>)>;

pub fn session_jsonl_signature(session_dir: &Path) -> io::Result<SessionJsonlSignature> {
    let read_dir = match fs::read_dir(session_dir) {
        Ok(read_dir) => read_dir,
        Err(error) if is_missing_path(&error) => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut signature: SessionJsonlSignature = Vec::new();
    for entry in read_dir {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.ends_with(".jsonl") {
            let metadata = entry.metadata()?;
            signature.push((name, metadata.len(), metadata.modified().ok()));
        }
    }
    signature.sort();
    Ok(signature)
}

pub fn is_missing_path(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::NotFound
}

fn parse_json_line(line: &str) -> Option<Value> {
    if line.trim().is_empty() {
        return None;
    }
    serde_json::from_str(line).ok()
}

/// Incremental form of [`session_jsonl_contains_message`] for the repeated ack poll: per file it
/// keeps the scanned byte offset and the envelope message ids seen in complete lines, so each
/// refresh reads only appended bytes. The unterminated tail is rescanned on growth, matching the
/// full scan, which also treats it as a line.
#[derive(Default)]
pub struct SessionAckScan {
    files: HashMap<String, FileAckScan>,
}

#[derive(Default)]
struct FileAckScan {
    offset: u64,
    residual: Vec<u8>,
    seen: HashSet<String>,
    tail_seen: HashSet<String>,
}

impl SessionAckScan {
    /// Reads what each `*.jsonl` gained since the last refresh. A missing dir clears the state.
    pub fn refresh(&mut self, session_dir: &Path) -> io::Result<()> {
        let read_dir = match fs::read_dir(session_dir) {
            Ok(read_dir) => read_dir,
            Err(error) if is_missing_path(&error) => {
                self.files.clear();
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        let mut names: Vec<String> = Vec::new();
        for entry in read_dir {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".jsonl") {
                names.push(name);
            }
        }
        self.files.retain(|name, _| names.contains(name));
        for name in names {
            let path = session_dir.join(&name);
            let len = match fs::metadata(&path) {
                Ok(metadata) => metadata.len(),
                Err(error) if is_missing_path(&error) => {
                    self.files.remove(&name);
                    continue;
                }
                Err(error) => return Err(error),
            };
            let state = self.files.entry(name).or_default();
            if len < state.offset {
                *state = FileAckScan::default();
            }
            if len == state.offset {
                continue;
            }
            let mut file = fs::File::open(&path)?;
            file.seek(SeekFrom::Start(state.offset))?;
            let read = file.take(len - state.offset).read_to_end(&mut state.residual)?;
            state.offset += read as u64;
            let complete = state.residual.iter().rposition(|byte| *byte == b'\n').map_or(0, |at| at + 1);
            for line in state.residual[..complete].split(|byte| *byte == b'\n') {
                collect_line_marker_ids(line, &mut state.seen);
            }
            state.residual.drain(..complete);
            state.tail_seen.clear();
            collect_line_marker_ids(&state.residual, &mut state.tail_seen);
        }
        Ok(())
    }

    /// Whether the last refresh saw a peer_message envelope for `message_id` in any file.
    pub fn contains(&self, message_id: &str) -> bool {
        self.files
            .values()
            .any(|state| state.seen.contains(message_id) || state.tail_seen.contains(message_id))
    }
}

const MARKER_PREFIX: &str = "messageId=\"";

/// Ids `X` for which a string value holding `<peer_message ` also holds `messageId="X"`; the same
/// predicate [`session_jsonl_contains_message`] applies per id (ids never contain a quote).
fn collect_line_marker_ids(raw: &[u8], out: &mut HashSet<String>) {
    let line = String::from_utf8_lossy(raw);
    // The JSON line escapes the quote after `=`, so pre-filter on the unquoted prefix.
    if !line.contains("messageId=") {
        return;
    }
    if let Some(value) = parse_json_line(&line) {
        collect_value_marker_ids(&value, out);
    }
}

fn collect_value_marker_ids(value: &Value, out: &mut HashSet<String>) {
    match value {
        Value::String(text) if text.contains("<peer_message ") => {
            let mut rest = text.as_str();
            while let Some(start) = rest.find(MARKER_PREFIX) {
                rest = &rest[start + MARKER_PREFIX.len()..];
                if let Some(end) = rest.find('"') {
                    out.insert(rest[..end].to_string());
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|entry| collect_value_marker_ids(entry, out)),
        Value::Object(map) => map.values().for_each(|entry| collect_value_marker_ids(entry, out)),
        _ => {}
    }
}

fn contains_envelope_marker(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text.contains("<peer_message ") && text.contains(needle),
        Value::Array(items) => items
            .iter()
            .any(|entry| contains_envelope_marker(entry, needle)),
        Value::Object(map) => map
            .values()
            .any(|entry| contains_envelope_marker(entry, needle)),
        _ => false,
    }
}
