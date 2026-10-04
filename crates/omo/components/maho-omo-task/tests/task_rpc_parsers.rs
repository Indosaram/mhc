use maho_omo_task::task_rpc_codec::{parse_task_cancel,parse_task_output,parse_task_send};
use serde_json::json;

#[test] fn send_preserves_text_and_enforces_utf16_boundaries() {
    use senpi_task::tools::control::send_schema::TaskSendMessage;
    for text in [" x ".into(),"x".repeat(32000),"😀".repeat(16000)] {
        let result=parse_task_send(&json!({"to":"st_A-0_","message":text})).expect("valid send");
        assert_eq!(result.to,"st_A-0_"); assert_eq!(result.message,Some(TaskSendMessage::Plain(text)));
    }
    for message in [json!(null),json!(false),json!(""),json!(" \n "),json!("x".repeat(32001)),json!(format!("{}x","😀".repeat(16000)))] { assert!(parse_task_send(&json!({"to":"st_a","message":message})).is_err()); }
    assert!(parse_task_send(&json!({"to":"st_a"})).is_err());
}
#[test] fn optional_control_fields_remain_optional_and_reject_wrong_types() {
    assert_eq!(parse_task_cancel(&json!({"task_id":"st_a"})).expect("omitted").reason,None);
    assert_eq!(parse_task_cancel(&json!({"task_id":"st_a","reason":""})).expect("empty").reason.as_deref(),Some(""));
    for reason in [json!(null),json!(true),json!(1),json!([]),json!({})] { assert!(parse_task_cancel(&json!({"task_id":"st_a","reason":reason})).is_err()); }
    let result=parse_task_output(&json!({"task_id":"st_a"})).expect("omitted"); assert_eq!(result.mode,None); assert_eq!(result.tail_lines,None);
    for mode in ["status","tail","full"] { assert!(parse_task_output(&json!({"task_id":"st_a","mode":mode})).expect("mode").mode.is_some()); }
    for request in [json!({}),json!({"task_id":" \n ","to":" \n "}),json!({"task_id":false,"to":false})] { assert!(parse_task_cancel(&request).is_err()); assert!(parse_task_output(&request).is_err()); assert!(parse_task_send(&request).is_err()); }
}

#[test] fn output_accepts_integral_decimal_and_exponent_json_counts() {
    for (raw, expected) in [(r#"{"task_id":"st_a","tail_lines":1.0}"#,1), (r#"{"task_id":"st_a","tail_lines":1e3}"#,1000)] {
        let request=serde_json::from_str(raw).expect("wire JSON");
        assert_eq!(parse_task_output(&request).expect("integral wire count").tail_lines,Some(expected));
    }
}

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
