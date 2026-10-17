use maho_agent::harness::messages::{CustomMessage, CustomMessageContent};
use maho_agent::types::{AgentMessage, CustomAgentMessage};
use serde_json::json;

#[test]
fn custom_agent_messages_serialize_as_their_role_shaped_object() {
    let message = AgentMessage::Custom(CustomAgentMessage::Custom(CustomMessage {
        role: "custom".into(),
        custom_type: "note".into(),
        content: CustomMessageContent::Text("hi".into()),
        display: false,
        details: None,
        timestamp: 7,
    }));
    let value = serde_json::to_value(&message).expect("serializes");
    assert_eq!(value, json!({"role":"custom","customType":"note","content":"hi","display":false,"timestamp":7}));
    assert_eq!(serde_json::from_value::<AgentMessage>(value).expect("round-trips"), message);
}

#[test]
fn summary_messages_deserialize_by_role() {
    let compaction = json!({"role":"compactionSummary","summary":"s","tokensBefore":3,"timestamp":1});
    assert!(matches!(serde_json::from_value::<AgentMessage>(compaction).expect("compaction"), AgentMessage::Custom(CustomAgentMessage::CompactionSummary(_))));
    let branch = json!({"role":"branchSummary","summary":"s","fromId":null,"timestamp":1});
    assert!(matches!(serde_json::from_value::<AgentMessage>(branch).expect("branch"), AgentMessage::Custom(CustomAgentMessage::BranchSummary(_))));
}
