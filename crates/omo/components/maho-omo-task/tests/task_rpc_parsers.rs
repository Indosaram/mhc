use maho_omo_task::task_rpc_codec::{parse_task_cancel,parse_task_output,parse_task_send};
use serde_json::json;

#[test] fn every_control_rejects_invalid_task_identity_and_nonobject_requests() {
    for id in [json!(null),json!(1),json!(""),json!("st_"),json!("foreign"),json!("st_a/b"),json!("st_é"),json!(format!("st_{}","x".repeat(254)))] {
        assert!(parse_task_send(&json!({"to":id,"message":"work"})).is_err());
        assert!(parse_task_cancel(&json!({"task_id":id})).is_err());
        assert!(parse_task_output(&json!({"task_id":id})).is_err());
    }
    for value in [json!(null),json!([]),json!("request")] { assert!(parse_task_send(&value).is_err()); assert!(parse_task_cancel(&value).is_err()); assert!(parse_task_output(&value).is_err()); }
    let id=format!("st_{}","x".repeat(253)); assert!(parse_task_send(&json!({"to":id,"message":"work"})).is_ok()); assert!(parse_task_cancel(&json!({"task_id":id})).is_ok()); assert!(parse_task_output(&json!({"task_id":id})).is_ok());
}
#[test] fn cancellation_reason_uses_utf16_limit_without_trimming_valid_text() {
    let reason="😀".repeat(1000); let parsed=parse_task_cancel(&json!({"task_id":"st_a","reason":reason})).expect("boundary"); assert_eq!(parsed.reason.as_deref(),Some(reason.as_str()));
    assert!(parse_task_cancel(&json!({"task_id":"st_a","reason":format!("{reason}x")})).is_err()); assert_eq!(parse_task_cancel(&json!({"task_id":"st_a","reason":" "})).expect("blank allowed").reason.as_deref(),Some(" "));
}
#[test] fn output_rejects_every_invalid_tail_count_and_explicit_null_mode() {
    for count in [json!(null),json!(0),json!(-1),json!(1.5),json!("1"),json!(1001)] { assert!(parse_task_output(&json!({"task_id":"st_a","tail_lines":count})).is_err()); }
    assert!(parse_task_output(&json!({"task_id":"st_a","mode":null})).is_err()); assert_eq!(parse_task_output(&json!({"task_id":"st_a","tail_lines":1})).expect("minimum").tail_lines,Some(1));
}
