use serde_json::{Value, json};

/// senpi 903b8336 kernel-death.ts `restartNotice` (no lost-globals list: only the JS kernel remains,
/// and the parked JS kernel reports none).
pub fn restart_notice(language: crate::tool::types::EvalLanguage, reason: &str) -> String {
    let language = match language {
        crate::tool::types::EvalLanguage::Js => "js",
    };
    format!("[{language} kernel was restarted after {reason}; every global is lost]")
}

pub fn unstarted_result(cell_id: &str, message: &str) -> Value {
    json!({"type":"result","cellId":cell_id,"ok":false,"error":{"message":message},"durationMs":0,"kernelState":"not-run"})
}
