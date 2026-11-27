use serde_json::{Value, json};

/// Builds a serializable error frame matching upstream `workerErrorFrame`
/// (`permission-worker-frames.ts`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerErrorFrame {
    #[serde(rename = "type")]
    pub frame_type: &'static str,
    pub message: String,
    pub filename: String,
    pub lineno: u64,
    pub colno: u64,
}

pub fn worker_error_frame(
    message: impl Into<String>,
    filename: Option<&str>,
    lineno: Option<u64>,
    colno: Option<u64>,
) -> WorkerErrorFrame {
    WorkerErrorFrame {
        frame_type: "error",
        message: message.into(),
        filename: filename.unwrap_or_default().to_owned(),
        lineno: lineno.unwrap_or(0),
        colno: colno.unwrap_or(0),
    }
}

pub fn worker_messageerror_frame(reason_name: &str, reason_message: &str) -> Value {
    json!({
        "type": "messageerror",
        "message": format!("Worker message could not be forwarded ({reason_name}): {reason_message}"),
    })
}
