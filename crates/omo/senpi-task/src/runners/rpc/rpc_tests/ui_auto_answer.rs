//! `runners/rpc/ui-auto-answer.test.ts`.

use serde_json::json;

use crate::runners::rpc::ui_auto_answer::build_auto_ui_response;

#[test]
fn confirm_is_denied() {
    let request = json!({ "type": "extension_ui_request", "id": "u1", "method": "confirm", "title": "t", "message": "m" });
    assert_eq!(
        build_auto_ui_response(&request),
        Some(json!({ "type": "extension_ui_response", "id": "u1", "confirmed": false }))
    );
}

#[test]
fn select_input_editor_are_cancelled() {
    for request in [
        json!({ "type": "extension_ui_request", "id": "s", "method": "select", "title": "t", "options": ["a", "b"] }),
        json!({ "type": "extension_ui_request", "id": "i", "method": "input", "title": "t" }),
        json!({ "type": "extension_ui_request", "id": "e", "method": "editor", "title": "t" }),
    ] {
        assert_eq!(
            build_auto_ui_response(&request),
            Some(
                json!({ "type": "extension_ui_response", "id": request["id"], "cancelled": true })
            )
        );
    }
}

#[test]
fn display_only_requests_get_no_response() {
    let notify =
        json!({ "type": "extension_ui_request", "id": "n", "method": "notify", "message": "hi" });
    let set_status = json!({ "type": "extension_ui_request", "id": "st", "method": "setStatus", "statusKey": "k", "statusText": "v" });
    assert_eq!(build_auto_ui_response(&notify), None);
    assert_eq!(build_auto_ui_response(&set_status), None);
}
