//! Pause command wire validation, mirrored from pause-command-validation.ts.
use serde_json::Value;
fn non_empty(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|text| !text.is_empty() && text.encode_utf16().count() <= 256)
}
fn seq(value: &Value) -> bool {
    value
        .as_f64()
        .is_some_and(|n| n.is_finite() && n >= 0. && n.fract() == 0.)
}
pub fn validate_pause_command(value: &Value) -> Option<&'static str> {
    let kind = value["type"].as_str().unwrap_or_default();
    if matches!(kind, "abort" | "interrupt")
        && value.get("intent").is_some()
        && !matches!(value["intent"].as_str(), Some("stop" | "abort"))
    {
        return Some("intent must be \"stop\" or \"abort\".");
    }
    if kind == "resume_session" {
        if value.get("expectedSeq").is_some() && !seq(&value["expectedSeq"]) {
            return Some("expectedSeq must be a non-negative integer.");
        }
        if value.get("mode").is_some()
            && !matches!(value["mode"].as_str(), Some("continue" | "release"))
        {
            return Some("mode must be \"continue\" or \"release\".");
        }
        if value.get("message").is_some() && !value["message"].is_string() {
            return Some("message must be a string.");
        }
        if value.get("effectId").is_some() && !non_empty(&value["effectId"]) {
            return Some("effectId must be 1-256 characters.");
        }
    }
    if kind == "hold_input" {
        if !non_empty(&value["inputId"]) || !seq(&value["revision"]) || !value["text"].is_string() {
            return Some(
                "hold_input needs inputId (1-256 chars), a non-negative integer revision, and text.",
            );
        }
        if value.get("parentSessionId").is_some() && !non_empty(&value["parentSessionId"]) {
            return Some("parentSessionId must be 1-256 characters.");
        }
        if value.get("origin").is_some() && value["origin"] != "human" {
            return Some("origin must be \"human\".");
        }
    }
    if kind == "withdraw_held_input" && !non_empty(&value["inputId"]) {
        return Some("withdraw_held_input needs inputId.");
    }
    if kind == "pause_for_run"
        && (!non_empty(&value["parentSessionId"])
            || !seq(&value["parentPauseSeq"])
            || !matches!(
                value["runRef"]["kind"].as_str(),
                Some("dag" | "task" | "workpool" | "team")
            )
            || !non_empty(&value["runRef"]["id"]))
    {
        return Some(
            "pause_for_run needs parentSessionId, a non-negative integer parentPauseSeq, and runRef { kind, id }.",
        );
    }
    if kind == "resume_for_run"
        && (!non_empty(&value["parentSessionId"])
            || !seq(&value["parentPauseSeq"])
            || !non_empty(&value["resumeId"])
            || !seq(&value["childSeqCutoff"]))
    {
        return Some(
            "resume_for_run needs parentSessionId, parentPauseSeq, resumeId, and childSeqCutoff.",
        );
    }
    None
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_run_and_input_sequences() {
        assert!(validate_pause_command(&json!({"type":"pause_for_run","parentSessionId":"p","parentPauseSeq":0,"runRef":{"kind":"task","id":"t"}})).is_none());
        for revision in [json!(-1), json!(0.5), json!(null)] {
            assert!(
                validate_pause_command(
                    &json!({"type":"hold_input","inputId":"i","revision":revision,"text":"hi"})
                )
                .is_some()
            );
        }
    }
}
