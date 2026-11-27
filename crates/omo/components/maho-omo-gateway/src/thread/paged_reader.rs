//! Paged context tail reader (`thread/paged-reader.ts`).

use super::errors::{ThreadToolFailure, thread_tool_failure};
use super::read_budget::take_within_budget;
use super::reader::{
    THREAD_READ_DEFAULT_BYTES, THREAD_TAIL_DEFAULT_ITEMS, ThreadTranscriptEntry, base64url_encode,
    decode_base64url,
};

pub const CONTEXT_TAIL_MESSAGES: usize = THREAD_TAIL_DEFAULT_ITEMS;
const CURSOR_VERSION: &str = "t3";

#[derive(Debug, Clone)]
pub struct TailMessage {
    pub id: String,
    pub message: ThreadTranscriptEntry,
}

#[derive(Debug, Clone)]
pub struct ContextTail {
    pub messages: Vec<TailMessage>,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EntryPageRead {
    Ok {
        messages: Vec<ThreadTranscriptEntry>,
        truncated: bool,
        next_cursor: Option<String>,
    },
    Error {
        error: ThreadToolFailure,
    },
    Unsupported,
}

fn encode_cursor(revision: &str, before: &str, floor: &str) -> String {
    let raw = format!("{CURSOR_VERSION}\n{revision}\n{before}\n{floor}");
    base64url_encode(raw.as_bytes())
}

fn decode_cursor(raw: &str) -> Option<(String, String, String)> {
    let decoded = String::from_utf8(decode_base64url(raw)?).ok()?;
    let parts: Vec<&str> = decoded.split('\n').collect();
    if parts.len() != 4 || parts[0] != CURSOR_VERSION {
        return None;
    }
    if parts[2].is_empty() || parts[3].is_empty() {
        return None;
    }
    Some((
        parts[1].to_owned(),
        parts[2].to_owned(),
        parts[3].to_owned(),
    ))
}

pub fn is_entry_cursor(raw: &str) -> bool {
    if decode_cursor(raw).is_some() {
        return true;
    }
    let Some(bytes) = decode_base64url(raw) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes);
    text.split('\n').next() == Some("t2")
}

pub fn stale_entry_cursor() -> EntryPageRead {
    EntryPageRead::Error {
        error: thread_tool_failure(
            "cursor_stale",
            "The transcript changed after this cursor was issued, so its position no longer names the same entries.",
            "Re-read the thread without a cursor to restart from a current revision.",
            None,
        ),
    }
}

pub fn select_entry_page(
    tail: &ContextTail,
    max_bytes: Option<usize>,
    cursor: Option<&str>,
) -> EntryPageRead {
    let messages = &tail.messages;
    let mut floor = messages.len().saturating_sub(CONTEXT_TAIL_MESSAGES);
    let mut end = messages.len();
    if let Some(raw) = cursor {
        let Some((rev, before_id, floor_id)) = decode_cursor(raw) else {
            return stale_entry_cursor();
        };
        if rev != tail.revision {
            return stale_entry_cursor();
        }
        floor = match messages.iter().position(|m| m.id == floor_id) {
            Some(idx) => idx,
            None => return stale_entry_cursor(),
        };
        end = match messages.iter().position(|m| m.id == before_id) {
            Some(idx) => idx,
            None => return stale_entry_cursor(),
        };
        if end < floor {
            return stale_entry_cursor();
        }
    }
    let slice: Vec<ThreadTranscriptEntry> = messages[floor..end]
        .iter()
        .map(|m| m.message.clone())
        .collect();
    let (selected, _) =
        take_within_budget(&slice, max_bytes.unwrap_or(THREAD_READ_DEFAULT_BYTES), true);
    let start = end.saturating_sub(selected.len());
    let truncated = start > floor;
    let next_cursor = if truncated {
        match (messages.get(start), messages.get(floor)) {
            (Some(b), Some(f)) => Some(encode_cursor(&tail.revision, &b.id, &f.id)),
            _ => None,
        }
    } else {
        None
    };
    EntryPageRead::Ok {
        messages: selected,
        truncated,
        next_cursor,
    }
}
