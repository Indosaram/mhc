//! senpi `modes/rpc/legacy-record-depth.ts`: iterative JSON nesting guard, reply fallback
//! (`response_unwritable`), and refusal accounting for legacy wire output.

use serde_json::{Map, Value};

/// Stable error code for replies that could not be serialized or written (senpi#5702).
pub const RPC_ERROR_RESPONSE_UNWRITABLE: &str = "response_unwritable";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("legacy record exceeds depth limit: {detail}")]
pub struct LegacyRecordDepthError {
    pub detail: String,
}

impl LegacyRecordDepthError {
    pub fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LegacyEnqueueError {
    #[error(transparent)]
    Depth(#[from] LegacyRecordDepthError),
    #[error("unwritable legacy record: {0}")]
    Unwritable(String),
    #[error("{0}")]
    Other(String),
}

/// True when the JSON `text` nests arrays and objects deeper than `limit` (the record itself is
/// level 1), scanned without recursion. A text of at most `2 * limit` bytes cannot, so short
/// records cost one length compare.
pub fn json_text_deeper_than(text: &str, limit: usize) -> bool {
    if text.len() <= 2 * limit {
        return false;
    }
    let bytes = text.as_bytes();
    let mut depth: usize = 0;
    let mut in_string = false;
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if byte == b'\\' {
                index += 1;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'[' || byte == b'{' {
            depth += 1;
            if depth > limit {
                return true;
            }
        } else if byte == b']' || byte == b'}' {
            depth = depth.saturating_sub(1);
        }
        index += 1;
    }
    false
}

fn cause_line(cause: &str) -> &str {
    cause.split('\n').next().unwrap_or(cause)
}

/// The legacy records this writer refused or answered for, each reported once on stderr like other
/// refusals.
#[derive(Debug, Default)]
pub struct LegacyRecordRefusals {
    pub count: usize,
}

impl LegacyRecordRefusals {
    pub fn report(&mut self, session_id: &str, refusal: &LegacyRecordDepthError) {
        self.count += 1;
        eprintln!(
            "senpi rpc host: legacy record refused: {} (session {session_id})",
            refusal.detail
        );
    }

    pub fn report_reply(&mut self, session_id: &str, command: &str, cause: &str) {
        self.count += 1;
        eprintln!(
            "senpi rpc host: {command} response unwritable ({}); answered {RPC_ERROR_RESPONSE_UNWRITABLE} (session {session_id})",
            cause_line(cause)
        );
    }
}

fn is_reply(record: &Value) -> bool {
    record.get("type").and_then(Value::as_str) == Some("response")
}

/// The typed answer that replaces a reply that could not be written (senpi#5702): same `id` and
/// `command`, so the caller is answered instead of waiting for a line that never comes.
pub fn unwritable_reply(record: &Value, cause: &str) -> Value {
    let command = record
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut map = Map::new();
    if let Some(id) = record.get("id")
        && (id.is_string() || id.is_number())
    {
        map.insert("id".into(), id.clone());
    }
    map.insert("type".into(), Value::String("response".into()));
    map.insert("command".into(), Value::String(command.to_owned()));
    map.insert("success".into(), Value::Bool(false));
    map.insert(
        "error".into(),
        Value::String(format!(
            "{command} response could not be written: {}",
            cause_line(cause)
        )),
    );
    map.insert(
        "errorCode".into(),
        Value::String(RPC_ERROR_RESPONSE_UNWRITABLE.into()),
    );
    Value::Object(map)
}

pub trait LegacyRecordWriter {
    fn enqueue(&mut self, session_id: &str, record: &Value) -> Result<bool, LegacyEnqueueError>;
    fn legacy_refusals(&mut self) -> &mut LegacyRecordRefusals;
}

/// Queues one session record on the legacy wire. A reply that cannot be written is answered with
/// `response_unwritable`; a non-reply that hits the depth limit is refused and reported without
/// throwing, returning `Ok(true)` so the session keeps running.
pub fn enqueue_legacy_record<W: LegacyRecordWriter>(
    writer: &mut W,
    session_id: &str,
    record: &Value,
) -> Result<bool, LegacyEnqueueError> {
    match writer.enqueue(session_id, record) {
        Ok(accepted) => Ok(accepted),
        Err(cause) => {
            let cause_text = cause.to_string();
            if is_reply(record) {
                let answer = unwritable_reply(record, &cause_text);
                let command = answer
                    .get("command")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                writer
                    .legacy_refusals()
                    .report_reply(session_id, &command, &cause_text);
                return writer.enqueue(session_id, &answer);
            }
            match cause {
                LegacyEnqueueError::Depth(refusal) => {
                    writer.legacy_refusals().report(session_id, &refusal);
                    Ok(true)
                }
                other => Err(other),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_text_deeper_than_scans_without_recursion_and_ignores_string_brackets() {
        assert!(!json_text_deeper_than(r#"{"a":"[[[[]]]]"}"#, 2));
        assert!(!json_text_deeper_than("[[]]", 2));
        assert!(json_text_deeper_than("[[[]]]", 2));
        assert!(!json_text_deeper_than(r#"{"a":"\"[[{"}"#, 1));
    }

    #[test]
    fn enqueue_answers_unwritable_replies_and_swallows_depth_refusals() {
        struct FakeWriter {
            fail_first: bool,
            sent: Vec<Value>,
            refusals: LegacyRecordRefusals,
        }
        impl LegacyRecordWriter for FakeWriter {
            fn enqueue(
                &mut self,
                _session_id: &str,
                record: &Value,
            ) -> Result<bool, LegacyEnqueueError> {
                if self.fail_first {
                    self.fail_first = false;
                    return Err(LegacyEnqueueError::Depth(LegacyRecordDepthError::new(
                        "too deep",
                    )));
                }
                self.sent.push(record.clone());
                Ok(true)
            }
            fn legacy_refusals(&mut self) -> &mut LegacyRecordRefusals {
                &mut self.refusals
            }
        }

        let mut writer = FakeWriter {
            fail_first: true,
            sent: Vec::new(),
            refusals: LegacyRecordRefusals::default(),
        };
        let reply = json!({"type":"response","id":"req-1","command":"get_state"});
        assert!(enqueue_legacy_record(&mut writer, "s1", &reply).expect("answered"));
        assert_eq!(writer.refusals.count, 1);
        assert_eq!(writer.sent.len(), 1);
        assert_eq!(writer.sent[0]["errorCode"], RPC_ERROR_RESPONSE_UNWRITABLE);
        assert_eq!(writer.sent[0]["id"], "req-1");

        writer.fail_first = true;
        let event = json!({"type":"agent_end"});
        assert!(enqueue_legacy_record(&mut writer, "s1", &event).expect("consumed"));
        assert_eq!(writer.refusals.count, 2);
        assert_eq!(writer.sent.len(), 1);
    }
}
