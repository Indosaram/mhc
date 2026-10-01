use maho_server::app_server::projection_message_items::*;
use serde_json::json;
use std::sync::Arc;
fn projector() -> MessageItemProjector {
    MessageItemProjector::new(MessageItemNotifier {
        item_id: Arc::new(|index| format!("item-{index}")),
        started: Arc::new(|item| json!({"method":"item/started","params":{"item":item}})),
        completed: Arc::new(|item| json!({"method":"item/completed","params":{"item":item}})),
        notification: Arc::new(|method, params| json!({"method":method,"params":params})),
    })
}
#[test]
fn dangling_completion_uses_accumulated_text_and_insertion_order_once() {
    let mut projector = projector();
    projector.delta_text(2, "first");
    projector.delta_text(0, "second");
    projector.delta_text(2, " plus");
    projector.delta_reasoning(1, "reason");
    let completed = projector.close_dangling_items();
    assert_eq!(completed.iter().map(|message| message["params"]["item"]["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["item-2", "item-0", "item-1"]);
    assert_eq!(completed[0]["params"]["item"]["text"], "first plus");
    assert_eq!(completed[2]["params"]["item"]["content"], json!(["reason"]));
    assert!(projector.close_dangling_items().is_empty());
}
#[test]
fn final_assistant_message_completes_only_unfinished_text_and_reasoning() {
    let mut projector = projector();
    projector.start_text(0);
    projector.complete_text(0, "already");
    let notifications = projector.complete_dangling_text(&json!({"content":[{"type":"text","text":"replacement"},{"type":"thinking","thinking":"thought"},{"type":"toolCall","id":"tool"},{"type":"text","text":"final"}]}));
    assert_eq!(notifications.len(), 2);
    assert_eq!(notifications[0]["params"]["item"]["content"], json!(["thought"]));
    assert_eq!(notifications[1]["params"]["item"]["text"], "final");
    assert!(projector.close_dangling_items().is_empty());
}
