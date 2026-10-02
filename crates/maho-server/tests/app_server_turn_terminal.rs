use maho_server::app_server::turn_terminal::turn_terminal_notifications;
use serde_json::json;

#[test]
fn failed_turn_emits_error_before_completion_but_interrupted_turn_does_not() {
    let failed = turn_terminal_notifications("thread",json!({"id":"turn","status":"failed","error":{"message":"failed"}}));
    assert_eq!(failed.len(),2);
    assert_eq!(failed[0]["method"],"error");
    assert_eq!(failed[0]["params"]["willRetry"],false);
    assert_eq!(failed[1]["method"],"turn/completed");
    let interrupted = turn_terminal_notifications("thread",json!({"id":"turn","status":"interrupted","error":{"message":"aborted"}}));
    assert_eq!(interrupted.len(),1);
    assert_eq!(interrupted[0]["method"],"turn/completed");
}
