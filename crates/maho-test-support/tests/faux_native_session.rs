#![cfg(feature = "sdk")]

use maho_test_support::faux::{FauxResponse, FauxScript};
use maho_test_support::faux_session::FauxSession;

#[tokio::test]
async fn native_session_drives_prompt_reply_and_durable_entries() {
    let session = FauxSession::new(FauxScript {
        name: "native-hello".to_owned(),
        prompt: "hi".to_owned(),
        responses: vec![FauxResponse { content: "hello".to_owned(), stop_reason: "stop".to_owned() }],
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("native turn must settle").expect("native session");
    let messages = result["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "hi");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0]["text"], "hello");
    let entries = result["entries"].as_array().expect("entries");
    assert_eq!(entries.iter().filter(|entry| entry["type"] == "message").count(), 2);
    let events = result["events"].as_array().expect("events");
    assert_eq!(events.first().expect("start")["type"], "agent_start");
    assert_eq!(events.last().expect("end")["type"], "agent_end");
}
