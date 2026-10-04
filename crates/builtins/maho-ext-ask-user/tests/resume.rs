use maho_ext_ask_user::resume::find_dangling_questions;
use serde_json::json;
#[test]
fn recovery_preserves_reverse_branch_order_and_ignores_incomplete_foreign_calls() {
    let call = |id: &str, name: &str, incomplete: bool| json!({"type":"toolCall","id":id,"name":name,"incomplete":incomplete,"arguments":{"waitForAnswer":false,"questions":[{"header":"Choice","question":"Pick?","multiSelect":false}]}});
    let entries = [
        json!({"type":"message","message":{"role":"assistant","content":[call("old","ask_user_question",false)]}}),
        json!({"type":"message","message":{"role":"assistant","content":[call("first","ask_user_question",false),call("second","ask_user_question",false),call("partial","ask_user_question",true),call("foreign","other_tool",false)]}}),
    ];
    let dangling = find_dangling_questions(&entries);
    assert_eq!(dangling.iter().map(|question|question.tool_call_id.as_str()).collect::<Vec<_>>(), ["second","first","old"]);
}

#[test]
fn async_recovery_requires_successfully_accepted_pending_result() {
    let call = json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"r","name":"ask_user_question","arguments":{"waitForAnswer":false,"questions":[{"header":"Choice","question":"Pick?","multiSelect":false}]}}]}});
    for (is_error, accepted, status) in [(true,true,"pending"),(false,false,"pending"),(false,true,"answered")] {
        let result = json!({"type":"message","message":{"role":"toolResult","toolCallId":"r","isError":is_error,"details":{"accepted":accepted,"status":status}}});
        assert!(find_dangling_questions(&[call.clone(),result]).is_empty());
    }
    let settled = json!({"type":"message","message":{"role":"user","content":[{"type":"text","text":"[Answer to question r]\nbody"}]}});
    assert!(find_dangling_questions(&[call,settled]).is_empty());
}
#[test]
fn pending_async_result_recovers_until_settled(){let call=json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"r","name":"ask_user_question","arguments":{"waitForAnswer":false,"questions":[{"header":"Auth","question":"Pick?","multiSelect":false}]}}]}});let result=json!({"type":"message","message":{"role":"toolResult","toolCallId":"r","details":{"accepted":true,"status":"pending"}}});assert_eq!(find_dangling_questions(&[call.clone(),result.clone()]).len(),1);for terminal in [json!({"type":"custom","customType":"ask-user:settlement","data":{"requestId":"r"}}),json!({"type":"custom","customType":"ask-user:resumed","data":{"toolCallId":"r"}}),json!({"type":"message","message":{"role":"user","content":"[Answer to question r]\nbody"}})]{assert!(find_dangling_questions(&[call.clone(),result.clone(),terminal]).is_empty());}}
#[test]
fn result_blocks_waiting_call_recovery(){let call=json!({"type":"message","message":{"role":"assistant","content":[{"type":"toolCall","id":"r","name":"ask_user_question","arguments":{"waitForAnswer":true,"questions":[{"header":"Auth","question":"Pick?","multiSelect":false}]}}]}});assert_eq!(find_dangling_questions(std::slice::from_ref(&call)).len(),1);let result=json!({"type":"message","message":{"role":"toolResult","toolCallId":"r","details":{"accepted":true,"status":"pending"}}});assert!(find_dangling_questions(&[call,result]).is_empty());}
