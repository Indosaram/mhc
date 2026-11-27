//! Bounded transcript reader (`thread/reader.ts`).

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::errors::{ThreadToolFailure, thread_tool_failure};
use super::read_budget::take_within_budget;

pub const THREAD_READ_DEFAULT_BYTES: usize = 131_072;
pub const THREAD_READ_MAX_BYTES: usize = 1_048_576;
pub const THREAD_TAIL_DEFAULT_ITEMS: usize = 60;
pub const THREAD_TAIL_MAX_ITEMS: usize = 200;

pub type ThreadTranscriptEntry = serde_json::Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadReadMode {
    Status,
    Tail,
    Page,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReaderSourceKind {
    LiveHost,
    SessionJsonl,
}

pub enum ThreadReadSourceRef<'a> {
    Live {
        entries: Vec<ThreadTranscriptEntry>,
    },
    Jsonl {
        path: &'a Path,
        live_host_present: bool,
    },
}

#[derive(Debug, Clone, Default)]
pub struct ThreadReadOptions {
    pub mode: Option<ThreadReadMode>,
    pub tail_items: Option<usize>,
    pub max_bytes: Option<usize>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadTranscriptReadOk {
    pub source: ReaderSourceKind,
    pub source_incomplete: bool,
    pub mode: ThreadReadMode,
    pub items: Vec<ThreadTranscriptEntry>,
    pub total_items: usize,
    pub truncated: bool,
    pub next_cursor: Option<String>,
    pub encoded_bytes: usize,
    pub max_bytes: usize,
    pub skipped_lines: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ThreadTranscriptReadResult {
    Ok(ThreadTranscriptReadOk),
    Error(ThreadToolFailure),
}

const READ_CHUNK_BYTES: usize = 65_536;
const CURSOR_VERSION: &str = "t1";
const OLDER_CURSOR_VERSION: &str = "t1o";

fn encode_base64url(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as usize;
        let b1 = *chunk.get(1).unwrap_or(&0) as usize;
        let b2 = *chunk.get(2).unwrap_or(&0) as usize;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[(n >> 18) & 63] as char);
        out.push(TABLE[(n >> 12) & 63] as char);
        if chunk.len() > 1 {
            out.push(TABLE[(n >> 6) & 63] as char);
        }
        if chunk.len() > 2 {
            out.push(TABLE[n & 63] as char);
        }
    }
    out
}

pub(crate) fn decode_base64url(input: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    };
    let raw = input.trim_end_matches('=').as_bytes();
    if raw.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity((raw.len() * 3) / 4);
    for chunk in raw.chunks(4) {
        let mut n = 0u32;
        for (i, &b) in chunk.iter().enumerate() {
            n |= val(b)? << (18 - i * 6);
        }
        out.push(((n >> 16) & 0xff) as u8);
        if chunk.len() > 2 {
            out.push(((n >> 8) & 0xff) as u8);
        }
        if chunk.len() > 3 {
            out.push((n & 0xff) as u8);
        }
    }
    Some(out)
}

fn load_jsonl_file(path: &Path) -> Option<(Vec<ThreadTranscriptEntry>, usize)> {
    let mut file = File::open(path).ok()?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; READ_CHUNK_BYTES];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return Some((Vec::new(), 0)),
        }
    }
    let text = String::from_utf8_lossy(&buf);
    let mut entries = Vec::new();
    let mut skipped_lines = 0usize;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(trimmed) {
            Ok(Value::Object(map)) => entries.push(map),
            _ => skipped_lines += 1,
        }
    }
    Some((entries, skipped_lines))
}

fn revision_of(entries: &[ThreadTranscriptEntry]) -> String {
    let mut hash: u32 = 2_166_136_261;
    for entry in entries {
        let id = entry
            .get("id")
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        let ts = entry
            .get("timestamp")
            .map(|v| match v {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        let identity = format!("{id}|{ts}");
        for code in identity.encode_utf16() {
            hash ^= code as u32;
            hash = hash.wrapping_mul(16_777_619);
        }
    }
    let mut rad36 = hash;
    let mut chars = Vec::new();
    if rad36 == 0 {
        chars.push('0');
    } else {
        while rad36 > 0 {
            let d = (rad36 % 36) as u8;
            chars.push(if d < 10 {
                (b'0' + d) as char
            } else {
                (b'a' + (d - 10)) as char
            });
            rad36 /= 36;
        }
        chars.reverse();
    }
    let h_str: String = chars.into_iter().collect();
    format!("{}.{}", entries.len(), h_str)
}

#[derive(Debug, Clone)]
enum Cursor {
    Forward {
        revision: String,
        offset: usize,
    },
    Older {
        revision: String,
        before: usize,
        floor: usize,
    },
}

fn encode_cursor(cursor: &Cursor) -> String {
    let text = match cursor {
        Cursor::Forward { revision, offset } => format!("{CURSOR_VERSION}:{revision}:{offset}"),
        Cursor::Older {
            revision,
            before,
            floor,
        } => {
            format!("{OLDER_CURSOR_VERSION}:{revision}:{before}:{floor}")
        }
    };
    encode_base64url(text.as_bytes())
}

fn decode_cursor(raw: &str) -> Option<Cursor> {
    if raw.is_empty() {
        return None;
    }
    let bytes = decode_base64url(raw)?;
    let decoded = String::from_utf8(bytes).ok()?;
    let mut parts = decoded.split(':');
    let version = parts.next()?;
    let revision = parts.next()?;
    let (rev_n, rev_h) = revision.split_once('.')?;
    if rev_n.is_empty()
        || !rev_n.bytes().all(|b| b.is_ascii_digit())
        || rev_h.is_empty()
        || !rev_h
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase())
    {
        return None;
    }
    let rest: Vec<&str> = parts.collect();
    if version == CURSOR_VERSION && rest.len() == 1 {
        let offset = rest[0].parse::<usize>().ok()?;
        return Some(Cursor::Forward {
            revision: revision.to_owned(),
            offset,
        });
    }
    if version == OLDER_CURSOR_VERSION && rest.len() == 2 {
        let before = rest[0].parse::<usize>().ok()?;
        let floor = rest[1].parse::<usize>().ok()?;
        if floor <= before {
            return Some(Cursor::Older {
                revision: revision.to_owned(),
                before,
                floor,
            });
        }
    }
    None
}

pub fn read_transcript(
    source: ThreadReadSourceRef<'_>,
    opts: &ThreadReadOptions,
) -> ThreadTranscriptReadResult {
    let mode = opts.mode.unwrap_or(ThreadReadMode::Tail);
    if let Some(tail_items) = opts.tail_items {
        if tail_items == 0 {
            return ThreadTranscriptReadResult::Error(thread_tool_failure(
                "invalid_arguments",
                "tail_items must be a positive whole number; received 0.",
                format!(
                    "Retry with tail_items between 1 and {THREAD_TAIL_MAX_ITEMS}, or omit it to take the default {THREAD_TAIL_DEFAULT_ITEMS}."
                ),
                None,
            ));
        }
        if tail_items > THREAD_TAIL_MAX_ITEMS {
            return ThreadTranscriptReadResult::Error(thread_tool_failure(
                "invalid_arguments",
                format!("tail_items {tail_items} exceeds the {THREAD_TAIL_MAX_ITEMS}-item cap."),
                format!(
                    "Retry with tail_items at most {THREAD_TAIL_MAX_ITEMS}, then page backward with the returned cursor if you need more."
                ),
                None,
            ));
        }
    }
    if let Some(max_bytes) = opts.max_bytes {
        if max_bytes == 0 {
            return ThreadTranscriptReadResult::Error(thread_tool_failure(
                "invalid_arguments",
                "max_bytes must be a positive whole number; received 0.",
                format!(
                    "Retry with max_bytes between 1 and {THREAD_READ_MAX_BYTES}, or omit it to take the default {THREAD_READ_DEFAULT_BYTES}."
                ),
                None,
            ));
        }
        if max_bytes > THREAD_READ_MAX_BYTES {
            return ThreadTranscriptReadResult::Error(thread_tool_failure(
                "message_too_large",
                format!("max_bytes {max_bytes} exceeds the {THREAD_READ_MAX_BYTES}-byte hard cap."),
                format!(
                    "Re-read with max_bytes at most {THREAD_READ_MAX_BYTES} and follow next_cursor for the rest of the transcript."
                ),
                None,
            ));
        }
    }

    let max_bytes = opts.max_bytes.unwrap_or(THREAD_READ_DEFAULT_BYTES);
    let tail_items = opts.tail_items.unwrap_or(THREAD_TAIL_DEFAULT_ITEMS);
    let cursor = match opts.cursor.as_deref() {
        Some(raw) => match decode_cursor(raw) {
            Some(c) => Some(c),
            None => {
                return ThreadTranscriptReadResult::Error(thread_tool_failure(
                    "cursor_invalid",
                    "The cursor is not a cursor this reader issued.",
                    "Re-read the thread without a cursor to get a fresh next_cursor, or call thread_list (tool.thread_list() in an eval cell) to confirm the thread address.",
                    None,
                ));
            }
        },
        None => None,
    };

    let (entries, skipped_lines, read_source_kind, source_incomplete) = match source {
        ThreadReadSourceRef::Live { entries } => (entries, 0, ReaderSourceKind::LiveHost, false),
        ThreadReadSourceRef::Jsonl {
            path,
            live_host_present,
        } => match load_jsonl_file(path) {
            Some((entries, skipped)) => (
                entries,
                skipped,
                ReaderSourceKind::SessionJsonl,
                !live_host_present,
            ),
            None => {
                return ThreadTranscriptReadResult::Error(thread_tool_failure(
                    "not_found",
                    format!("No session transcript exists at {}.", path.display()),
                    "Call thread_list (tool.thread_list() in an eval cell) to get a current thread value, then read that thread instead.",
                    None,
                ));
            }
        },
    };

    let revision = revision_of(&entries);
    if let Some(ref c) = cursor {
        let cur_rev = match c {
            Cursor::Forward { revision, .. } | Cursor::Older { revision, .. } => revision,
        };
        if cur_rev != &revision {
            return ThreadTranscriptReadResult::Error(thread_tool_failure(
                "cursor_stale",
                "The transcript changed after this cursor was issued, so its position no longer names the same entries.",
                "Re-read the thread without a cursor to restart from a current revision.",
                None,
            ));
        }
    }

    let total_items = entries.len();
    if mode == ThreadReadMode::Status {
        return ThreadTranscriptReadResult::Ok(ThreadTranscriptReadOk {
            source: read_source_kind,
            source_incomplete,
            mode,
            items: Vec::new(),
            total_items,
            truncated: false,
            next_cursor: None,
            encoded_bytes: 0,
            max_bytes,
            skipped_lines,
        });
    }

    if mode == ThreadReadMode::Page && !matches!(cursor, Some(Cursor::Older { .. })) {
        let start = match cursor {
            Some(Cursor::Forward { offset, .. }) => offset.min(total_items),
            _ => 0,
        };
        let (selected, encoded_bytes) = take_within_budget(&entries[start..], max_bytes, false);
        let next_offset = start + selected.len();
        let truncated = next_offset < total_items;
        let next_cursor = truncated.then(|| {
            encode_cursor(&Cursor::Forward {
                revision,
                offset: next_offset,
            })
        });
        return ThreadTranscriptReadResult::Ok(ThreadTranscriptReadOk {
            source: read_source_kind,
            source_incomplete,
            mode,
            items: selected,
            total_items,
            truncated,
            next_cursor,
            encoded_bytes,
            max_bytes,
            skipped_lines,
        });
    }

    let (floor, end) = match cursor {
        Some(Cursor::Older { before, floor, .. }) => {
            (floor.min(total_items), before.min(total_items))
        }
        _ => (total_items.saturating_sub(tail_items), total_items),
    };
    let slice = if floor <= end {
        &entries[floor..end]
    } else {
        &[]
    };
    let (selected, encoded_bytes) = take_within_budget(slice, max_bytes, true);
    let start = end.saturating_sub(selected.len());
    let truncated = start > floor;
    let next_cursor = truncated.then(|| {
        encode_cursor(&Cursor::Older {
            revision,
            before: start,
            floor,
        })
    });
    ThreadTranscriptReadResult::Ok(ThreadTranscriptReadOk {
        source: read_source_kind,
        source_incomplete,
        mode,
        items: selected,
        total_items,
        truncated,
        next_cursor,
        encoded_bytes,
        max_bytes,
        skipped_lines,
    })
}

pub fn base64url_encode(bytes: &[u8]) -> String {
    encode_base64url(bytes)
}

#[allow(dead_code)]
fn _unused_check(_m: HashMap<String, String>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_short_chunks_match_standard_vectors() {
        for (input, encoded) in [
            (&b""[..], ""),
            (&b"f"[..], "Zg"),
            (&b"fo"[..], "Zm8"),
            (&b"foo"[..], "Zm9v"),
            (&[0xfb, 0xff][..], "-_8"),
        ] {
            assert_eq!(base64url_encode(input), encoded);
            assert_eq!(decode_base64url(encoded).as_deref(), Some(input));
        }
    }

    #[test]
    fn tail_read_returns_newest_and_pages_older_entries() {
        let entries: Vec<ThreadTranscriptEntry> = (1..=5)
            .map(|i| {
                let mut m = ThreadTranscriptEntry::new();
                m.insert("id".into(), Value::String(format!("m{i}")));
                m.insert("role".into(), Value::String("user".into()));
                m.insert("content".into(), Value::String("x".repeat(40)));
                m
            })
            .collect();

        let first = match read_transcript(
            ThreadReadSourceRef::Live {
                entries: entries.clone(),
            },
            &ThreadReadOptions {
                mode: Some(ThreadReadMode::Tail),
                tail_items: Some(5),
                max_bytes: Some(150),
                cursor: None,
            },
        ) {
            ThreadTranscriptReadResult::Ok(ok) => ok,
            ThreadTranscriptReadResult::Error(e) => panic!("unexpected err: {e:?}"),
        };
        assert!(first.truncated);
        assert!(
            first
                .items
                .last()
                .and_then(|m| m.get("id"))
                .and_then(Value::as_str)
                == Some("m5")
        );
        let cursor = first.next_cursor.unwrap();

        let older = match read_transcript(
            ThreadReadSourceRef::Live { entries },
            &ThreadReadOptions {
                mode: Some(ThreadReadMode::Page),
                tail_items: None,
                max_bytes: Some(150),
                cursor: Some(cursor),
            },
        ) {
            ThreadTranscriptReadResult::Ok(ok) => ok,
            ThreadTranscriptReadResult::Error(e) => panic!("unexpected err: {e:?}"),
        };
        assert!(!older.items.is_empty());
        assert!(
            older
                .items
                .last()
                .unwrap()
                .get("id")
                .and_then(Value::as_str)
                < Some("m5")
        );
    }
}
