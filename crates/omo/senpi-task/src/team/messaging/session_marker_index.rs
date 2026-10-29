//! Incremental index over a lead's append-only session JSONL. The lead poller must check, many times
//! per tick, whether a peer-message envelope for a given messageId has been persisted. The naive
//! check re-read + re-split the ENTIRE file on every call (O(pending x fileSize) per tick). This index
//! keeps a per-path byte offset and the set of messageIds already seen, so each check reads only the
//! bytes appended since the last check (and reads NOTHING when the file has not grown).

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use regex::Regex;

/// Reads the byte range [start, end) of a session JSONL file. Injectable so tests can count bytes.
pub type SessionSliceReader = Box<dyn Fn(&Path, u64, u64) -> io::Result<String> + Send + Sync>;
pub type SessionMarkerExtractor = Box<dyn Fn(&str) -> Vec<String> + Send + Sync>;

struct PathState {
    offset: u64,
    residual: String,
    seen: HashSet<String>,
}

impl PathState {
    fn fresh() -> Self {
        Self {
            offset: 0,
            residual: String::new(),
            seen: HashSet::new(),
        }
    }
}

// messageId="..." inside a persisted peer_message envelope. The envelope lives inside JSON-encoded
// strings, so the quote before the id may be a raw " or an escaped \". Match both.
static MESSAGE_ID_MARKER: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"<peer_message [^>]*?messageId=\\?"([^"\\]+)\\?""#).ok());

pub struct SessionMarkerIndex {
    extract_markers: SessionMarkerExtractor,
    read_slice: SessionSliceReader,
    states: Mutex<HashMap<PathBuf, PathState>>,
}

pub fn create_session_marker_index(read_slice: Option<SessionSliceReader>) -> SessionMarkerIndex {
    create_incremental_session_marker_index(Box::new(extract_message_ids), read_slice)
}

pub fn create_incremental_session_marker_index(
    extract_markers: SessionMarkerExtractor,
    read_slice: Option<SessionSliceReader>,
) -> SessionMarkerIndex {
    SessionMarkerIndex {
        extract_markers,
        read_slice: read_slice.unwrap_or_else(|| Box::new(default_read_slice)),
        states: Mutex::new(HashMap::new()),
    }
}

impl SessionMarkerIndex {
    pub fn contains(&self, path: Option<&Path>, message_id: &str) -> io::Result<bool> {
        let Some(path) = path else {
            return Ok(false);
        };
        let size = match std::fs::metadata(path) {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };

        let mut states = self.states.lock().unwrap_or_else(PoisonError::into_inner);
        let state = states.entry(path.to_path_buf()).or_insert_with(PathState::fresh);
        if size < state.offset {
            // The file shrank (rotation/truncation): start clean and rescan from zero.
            *state = PathState::fresh();
        }
        if size == state.offset {
            return Ok(state.seen.contains(message_id));
        }

        let chunk = (self.read_slice)(path, state.offset, size)?;
        state.offset = size;
        let text = std::mem::take(&mut state.residual) + &chunk;
        let complete = match text.rfind('\n') {
            Some(last_newline) => {
                state.residual = text[last_newline + 1..].to_string();
                &text[..=last_newline]
            }
            None => {
                state.residual.clone_from(&text);
                ""
            }
        };
        for marker in (self.extract_markers)(complete) {
            state.seen.insert(marker);
        }
        Ok(state.seen.contains(message_id))
    }
}

pub fn extract_message_ids(text: &str) -> Vec<String> {
    let Some(pattern) = MESSAGE_ID_MARKER.as_ref() else {
        return Vec::new();
    };
    pattern
        .captures_iter(text)
        .filter_map(|captures| captures.get(1).map(|id| id.as_str().to_string()))
        .collect()
}

pub fn default_read_slice(path: &Path, start: u64, end: u64) -> io::Result<String> {
    if end <= start {
        return Ok(String::new());
    }
    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(start))?;
    let mut buffer = Vec::new();
    file.take(end - start).read_to_end(&mut buffer)?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}
