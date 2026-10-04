use maho_server::app_server::turn_adapter::{turn_input_params,turn_interrupt_params};
use serde_json::json;

#[test]
fn input_adapter_normalizes_supported_wire_shapes_and_preserves_client_identity() {
    let params = turn_input_params(&json!({"params":{"threadId":"thread","expectedTurnId":"turn","clientUserMessageId":"client","input":[{"type":"text","text":"hello","text_elements":false,"extra":true},{"type":"image","url":"image"},{"type":"localImage","path":"local"},{"type":"skill","name":"skill","path":"path"},{"type":"mention","name":"mention","path":"path"}]}}),true).unwrap();
    assert_eq!(params.thread_id,"thread");assert_eq!(params.expected_turn_id.as_deref(),Some("turn"));assert_eq!(params.client_user_message_id.as_deref(),Some("client"));
    assert_eq!(params.input[0],json!({"type":"text","text":"hello","text_elements":[]}));
    assert_eq!(params.input.len(),5);
}
#[test]
fn invalid_input_adapter_shapes_are_rejected_before_thread_resolution() {
    for params in [json!(null),json!([]),json!({"threadId":""}),json!({"threadId":"thread","input":false}),json!({"threadId":"thread","clientUserMessageId":3,"input":[]}),json!({"threadId":"thread","input":[null]}),json!({"threadId":"thread","input":[{"type":"image"}]})] {
        assert_eq!(turn_input_params(&json!({"params":params}),false).err().unwrap().code,-32602);
    }
    assert_eq!(turn_interrupt_params(&json!({"params":{"threadId":"thread","turnId":"turn"}})).unwrap(),("thread".into(),"turn".into()));
    assert_eq!(turn_interrupt_params(&json!({"params":{"threadId":"thread","turnId":""}})).unwrap_err().code,-32602);
}
