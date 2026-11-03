//! What a child's transcript says about the steering deliveries written to it
//! (`docs/cleanroom/specs/task.md` Delta D7): a steer is admitted when the child's session file
//! has a `session_control_delivery` custom message whose `details.delivery_id` is the steer's id,
//! and answered when a completed assistant message follows it.
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How a steer fared in the child's transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// Recorded and followed by a completed assistant message.
    Answered,
    /// Recorded, but no assistant message came after it.
    Unanswered,
    /// Not in the transcript.
    NotAdmitted,
}

/// The admitted delivery ids of one child session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Transcript {
    /// Delivery id -> answered.
    admitted: HashMap<String, bool>,
}

impl Transcript {
    pub fn admission(&self, delivery_id: &str) -> Admission {
        match self.admitted.get(delivery_id) {
            Some(true) => Admission::Answered,
            Some(false) => Admission::Unanswered,
            None => Admission::NotAdmitted,
        }
    }

    /// Reads the session entries (JSON lines; the header and malformed lines are skipped).
    pub fn parse(text: &str) -> Self {
        let mut transcript = Self::default();
        for line in text.lines() {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            match entry.get("type").and_then(Value::as_str) {
                Some("custom_message")
                    if entry.get("customType").and_then(Value::as_str)
                        == Some(maho_gateway::DELIVERY_TYPE) =>
                {
                    if let Some(id) = entry
                        .get("details")
                        .and_then(|details| details.get("delivery_id"))
                        .and_then(Value::as_str)
                    {
                        transcript.admitted.entry(id.to_owned()).or_insert(false);
                    }
                }
                Some("message") if completed_assistant(entry.get("message")) => {
                    for answered in transcript.admitted.values_mut() {
                        *answered = true;
                    }
                }
                _ => {}
            }
        }
        transcript
    }
}

fn completed_assistant(message: Option<&Value>) -> bool {
    let Some(message) = message else {
        return false;
    };
    message.get("role").and_then(Value::as_str) == Some("assistant")
        && !matches!(
            message.get("stopReason").and_then(Value::as_str),
            Some("error" | "aborted")
        )
}

/// The session file of `session_id` under `sessions_dir` (`<timestamp>_<id>.jsonl` or
/// `<id>.jsonl`, directly or one directory below).
pub fn session_file(sessions_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let suffix = format!("_{session_id}.jsonl");
    let exact = format!("{session_id}.jsonl");
    let matches = |path: &Path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == exact || name.ends_with(&suffix))
    };
    let mut dirs = vec![sessions_dir.to_path_buf()];
    let mut depth = 0;
    while depth < 2 {
        let mut next = Vec::new();
        for dir in &dirs {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if path.is_dir() {
                    next.push(path);
                } else if matches(&path) {
                    return Some(path);
                }
            }
        }
        dirs = next;
        depth += 1;
    }
    None
}

/// The transcript of `session_id`; `None` when it cannot be found or read.
pub fn read_transcript(sessions_dir: &Path, session_id: &str) -> Option<Transcript> {
    let path = session_file(sessions_dir, session_id)?;
    let text = std::fs::read_to_string(path).ok()?;
    Some(Transcript::parse(&text))
}

/// The delivery id of a steer written to `path` (the gateway names inbox files
/// `<delivery id>.json`).
pub fn delivery_id_of(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .map(str::to_owned)
}

/// Follow-up text for steers the child recorded but never answered (their text is not repeated).
pub const ACT_ON_RECEIVED: &str = "Act on the gateway messages you already received in this session and have not answered yet, then reply with the final result.";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn delivery(id: &str) -> String {
        json!({"type": "custom_message", "id": "x", "parentId": null, "timestamp": "t",
            "customType": "session_control_delivery", "content": [], "display": true,
            "details": {"delivery_id": id, "source": "session_control"}})
        .to_string()
    }

    fn assistant(stop: &str) -> String {
        json!({"type": "message", "id": "y", "parentId": "x", "timestamp": "t",
            "message": {"role": "assistant", "content": [], "stopReason": stop}})
        .to_string()
    }

    #[test]
    fn admission_states() {
        let header = json!({"type": "session", "id": "sid"}).to_string();
        let text = [
            header,
            delivery("d1"),
            assistant("stop"),
            delivery("d2"),
            assistant("aborted"),
            "not json".into(),
        ]
        .join("\n");
        let transcript = Transcript::parse(&text);
        assert_eq!(transcript.admission("d1"), Admission::Answered);
        assert_eq!(transcript.admission("d2"), Admission::Unanswered);
        assert_eq!(transcript.admission("d3"), Admission::NotAdmitted);
    }

    #[test]
    fn finds_the_session_file() {
        let dir = tempfile::tempdir().expect("tmp");
        let nested = dir.path().join("project");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::write(nested.join("2026-01-01T00-00-00_abc.jsonl"), delivery("d")).expect("write");
        std::fs::write(dir.path().join("other_xyz.jsonl"), "").expect("write");
        let found = session_file(dir.path(), "abc").expect("found");
        assert!(found.ends_with("2026-01-01T00-00-00_abc.jsonl"));
        assert_eq!(
            read_transcript(dir.path(), "abc")
                .expect("read")
                .admission("d"),
            Admission::Unanswered
        );
        assert!(read_transcript(dir.path(), "missing").is_none());
        assert_eq!(
            delivery_id_of(Path::new("/i/abc123.json")).as_deref(),
            Some("abc123")
        );
    }
}
