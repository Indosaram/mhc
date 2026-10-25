//! Validated, fail-closed durable work custody records.
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObligationKind {
    Children,
    Background,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CustodyState {
    Open,
    Queued,
    Consumed,
    Settled,
    Held,
    Cancelled,
    Passive,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustodyPayload {
    pub message: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deliver_as: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkCustodyRecord {
    pub id: String,
    pub key: String,
    pub kind: ObligationKind,
    pub state: CustodyState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_count: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<CustodyPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_entry_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notice_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivery: Option<String>,
}
pub fn is_record(value: &Value) -> bool {
    value.is_object()
}
pub fn read_work_custody(value: &Value) -> Option<WorkCustodyRecord> {
    let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
    Some(WorkCustodyRecord {
        id: text("id")?,
        key: text("key")?,
        kind: serde_json::from_value(value.get("kind")?.clone()).ok()?,
        state: serde_json::from_value(value.get("state")?.clone()).ok()?,
        source: text("source"),
        active_count: value.get("activeCount").and_then(Value::as_f64),
        detail: text("detail"),
        ticket_id: text("ticketId"),
        payload: value.get("payload").and_then(read_payload),
        run_id: text("runId"),
        transcript_entry_id: text("transcriptEntryId"),
        notice_id: text("noticeId"),
        delivery: text("delivery").filter(|s| matches!(s.as_str(), "failed" | "retired")),
    })
}
pub fn read_payload(value: &Value) -> Option<CustodyPayload> {
    let message = value.get("message")?;
    message.get("customType")?.as_str()?;
    message.get("display")?.as_bool()?;
    if !is_message_content(message.get("content")?) {
        return None;
    }
    let mut clean = serde_json::json!({"customType":message["customType"],"content":message["content"],"display":message["display"]});
    if let Some(details) = message.get("details") {
        clean["details"] = details.clone();
    }
    Some(CustodyPayload {
        message: clean,
        deliver_as: value
            .get("deliverAs")
            .and_then(Value::as_str)
            .filter(|s| matches!(*s, "steer" | "followUp" | "nextTurn"))
            .map(str::to_owned),
    })
}
fn is_message_content(value: &Value) -> bool {
    value.is_string()
        || value.as_array().is_some_and(|parts| {
            parts
                .iter()
                .all(|part| match part.get("type").and_then(Value::as_str) {
                    Some("text") => part.get("text").is_some_and(Value::is_string),
                    Some("image") => {
                        part.get("data").is_some_and(Value::is_string)
                            && part.get("mimeType").is_some_and(Value::is_string)
                    }
                    _ => false,
                })
        })
}
pub fn custody_is_open(record: &WorkCustodyRecord) -> bool {
    matches!(
        record.state,
        CustodyState::Open | CustodyState::Queued | CustodyState::Consumed
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn unknown_states_and_kinds_fail_closed() {
        for state in [
            "open",
            "queued",
            "consumed",
            "settled",
            "held",
            "cancelled",
            "passive",
        ] {
            let record = read_work_custody(
                &json!({"id":"i","key":"k","kind":"children","state":state,"source":3}),
            )
            .unwrap();
            assert_eq!(
                custody_is_open(&record),
                matches!(state, "open" | "queued" | "consumed")
            );
            assert!(record.source.is_none());
        }
        assert!(
            read_work_custody(&json!({"id":"i","key":"k","kind":"children","state":"future"}))
                .is_none()
        );
        assert!(
            read_work_custody(&json!({"id":"i","key":"k","kind":"future","state":"open"}))
                .is_none()
        );
    }
    #[test]
    fn queued_payload_validates_every_content_part() {
        for content in [
            json!("text"),
            json!([]),
            json!([{"type":"text","text":"a"},{"type":"image","data":"a","mimeType":"image/png"}]),
        ] {
            assert!(
                read_payload(
                    &json!({"message":{"customType":"x","display":false,"content":content}})
                )
                .is_some()
            );
        }
        for content in [
            json!(3),
            json!([{"type":"text","text":false}]),
            json!([{"type":"image","data":"x"}]),
        ] {
            assert!(
                read_payload(
                    &json!({"message":{"customType":"x","display":false,"content":content}})
                )
                .is_none()
            );
        }
    }
}
