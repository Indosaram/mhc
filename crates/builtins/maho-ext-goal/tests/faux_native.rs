use std::sync::Arc;
use maho_ai::providers::faux::{FauxAssistantMessageOptions,faux_assistant_message};
use maho_ext_goal::{GoalExtension,types::{GoalStoreRef,GoalStatus}};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript,faux_session::FauxSession};

#[tokio::test]
async fn real_faux_session_accounts_existing_goal_through_native_factory() {
    let dir=tempfile::tempdir().unwrap();
    let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"fixture".into() };
    maho_ext_goal::store::create_goal(&reference,"work",None,0).await.unwrap();
    let stored=reference.clone();
    let extension=GoalExtension { reference:Arc::new(move |_|stored.clone()),now:Arc::new(||0.0) };
    let mut response=faux_assistant_message("done",FauxAssistantMessageOptions { timestamp:Some(0),..Default::default() });
    response.stop_reason=maho_ai::types::StopReason::ToolUse;
    response.content.push(maho_ai::types::ContentBlock::ToolCall(serde_json::from_value(serde_json::json!({"id":"finalize","name":"update_goal","arguments":{"status":"complete"}})).unwrap()));
    let session=FauxSession::new(FauxScript { name:"goal-native-accounting".into(),prompt:"work".into(),responses:Vec::new() })
        .with_native_extension(NativeExtensionFactory { path:"<goal>".into(),source_info:Default::default(),extension:Box::new(extension) })
        .with_native_responses(vec![response]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    assert!(result["messages"].as_array().unwrap().iter().any(|message|message["role"]=="assistant"));
    let goal=maho_ext_goal::store::read_goal(&reference).unwrap().unwrap();
    let actual_usage=result["messages"].as_array().unwrap().iter().filter(|message|message["role"]=="assistant").map(|message|message["usage"]["totalTokens"].as_u64().unwrap()).sum::<u64>();
    assert!(actual_usage>0); assert_eq!(goal.tokens_used,actual_usage);
    assert_eq!(goal.status,GoalStatus::Complete);
}
