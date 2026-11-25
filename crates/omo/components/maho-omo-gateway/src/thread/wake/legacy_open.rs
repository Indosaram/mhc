//! Legacy host `open_session` helper for cold wake (`thread/wake/legacy-open.ts`).

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde_json::Value;

use super::super::live_surface::{RpcCallOptions, request_frame};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeOpenRequest {
    pub session_path: String,
    pub cwd: String,
}

#[derive(Debug, Clone)]
pub enum WakeOpenReply {
    Opened {
        session_id: String,
        state: Value,
        attached: bool,
    },
    Refused {
        code: String,
        retry_after_ms: Option<u64>,
    },
    Unreachable {
        message: String,
    },
}

pub type WakeOpen = Arc<dyn Fn(WakeOpenRequest) -> BoxFuture<WakeOpenReply> + Send + Sync>;

fn refusal_of(frame: &Value) -> WakeOpenReply {
    let code = frame
        .get("errorCode")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            frame
                .get("error")
                .and_then(Value::as_str)
                .map(|s| s.split(':').next().unwrap_or(s).trim().to_owned())
        })
        .unwrap_or_else(|| "unknown".to_owned());
    let retry_after_ms = frame
        .get("errorData")
        .and_then(Value::as_object)
        .and_then(|o| o.get("retry_after_ms"))
        .and_then(Value::as_u64);
    WakeOpenReply::Refused {
        code,
        retry_after_ms,
    }
}

pub fn classify_open_frame(frame: &Value) -> WakeOpenReply {
    if frame.get("success").and_then(Value::as_bool) == Some(true)
        && let Some(data) = frame.get("data").and_then(Value::as_object)
        && let Some(session_id) = data.get("sessionId").and_then(Value::as_str)
    {
        return WakeOpenReply::Opened {
            session_id: session_id.to_owned(),
            state: data.get("state").cloned().unwrap_or(Value::Null),
            attached: data.get("attached").and_then(Value::as_bool) == Some(true),
        };
    }
    refusal_of(frame)
}

pub fn create_legacy_wake_open(socket: String, key_root: Option<PathBuf>) -> WakeOpen {
    Arc::new(move |req: WakeOpenRequest| {
        let socket = socket.clone();
        let key_root = key_root.clone();
        Box::pin(async move {
            let cmd = serde_json::json!({
                "type": "open_session",
                "sessionPath": req.session_path,
                "cwd": req.cwd,
                "retain_on_disconnect": true,
            });
            match request_frame(
                &socket,
                cmd,
                RpcCallOptions {
                    rpc_host: true,
                    key_root,
                    ..Default::default()
                },
            )
            .await
            {
                Ok(frame) => classify_open_frame(&frame),
                Err(err) => WakeOpenReply::Unreachable { message: err },
            }
        })
    })
}
