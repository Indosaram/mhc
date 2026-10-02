use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, sync::Arc};
use maho_ai::{auth::types::*, types::{Context, Message, UserMessage, UserContent, StopReason, ContentBlock}};
use maho_ext_anthropic_subscription::stream::{stream_anthropic_subscription, StreamDeps};

struct Flow;
#[async_trait::async_trait]
impl OAuthAuth for Flow {
    fn name(&self) -> &str { "fixture" }
    async fn login(&self, _: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> { panic!("no login") }
    async fn refresh(&self, _: &OAuthCredential, _: &maho_ai::utils::abort::AbortSignal) -> anyhow::Result<OAuthCredential> { panic!("ambient never refreshes") }
    async fn to_auth(&self, _: &OAuthCredential) -> anyhow::Result<ModelAuth> { Ok(ModelAuth::default()) }
}

async fn turn(mut frame: serde_json::Value, resident: bool) -> maho_ai::types::AssistantMessage {
    let directory = tempfile::tempdir().expect("directory"); let executable = directory.path().join("claude");
    frame["user_message_uuid"] = serde_json::Value::Null;
    let script = format!("#!/usr/bin/python3\nimport json,sys\nf=json.loads(sys.stdin.readline())\nassert f['request']['subtype']=='initialize'\nprint(json.dumps({{'type':'control_response','response':{{'subtype':'success','request_id':f['request_id'],'response':{{}}}}}}),flush=True)\nf=json.loads(sys.stdin.readline())\nassert f['message']['content'][-1]['text']=='hello'\nif f.get('uuid'):\n print(json.dumps({{'type':'user','isReplay':True,'uuid':f['uuid']}}),flush=True)\nr=json.loads({})\nr['user_message_uuid']=f.get('uuid')\nprint(json.dumps(r),flush=True)\nfor line in sys.stdin: pass\n", serde_json::to_string(&frame.to_string()).expect("literal"));
    std::fs::write(&executable, script).expect("script"); std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let model = maho_ai::models_generated::MODELS["anthropic"].values().next().expect("model").clone();
    let registry = Arc::new(tokio::sync::Mutex::new(Default::default()));
    let options = resident.then(|| {
        let mut options = maho_ai::types::SimpleStreamOptions::default();
        options.stream.session_id = Some("resident".into()); options.stream.request.stream_kind = Some(maho_ai::types::StreamKind::Main); options
    });
    let stream = stream_anthropic_subscription(model, Context { messages: vec![Message::User(UserMessage { content: UserContent::Text("hello".into()), timestamp: 1 })], ..Default::default() }, options, StreamDeps { executable, cwd: directory.path().into(), agent_dir: directory.path().join("agent"), environment: BTreeMap::new(), settings: maho_ext_anthropic_subscription::settings::load(&serde_json::json!({}), &serde_json::Value::Null, &Default::default()), store: Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new()), refresh: Arc::new(Flow), registry: registry.clone(), now: Arc::new(|| 1) });
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result()).await.expect("bounded native turn").expect("output");
    if resident && output.stop_reason == StopReason::Error { assert!(registry.lock().await.entries.is_empty()); }
    registry.lock().await.close_all().await.expect("cleanup"); output
}

#[tokio::test]
async fn native_turn_handshakes_projects_prompt_and_bills_result() {
    let output = turn(serde_json::json!({"type":"result","subtype":"success","result":"native","usage":{"input_tokens":3,"output_tokens":4},"stop_reason":"end_turn"}), false).await;
    assert_eq!(output.stop_reason, StopReason::Stop, "{:?}", output.error_message);
    assert_eq!(output.usage.total_tokens, 7);
    assert!(matches!(&output.content[0], ContentBlock::Text(text) if text.text == "native"));
}

#[tokio::test]
async fn native_failure_bills_tokens_and_returns_error_event() {
    let output = turn(serde_json::json!({"type":"result","subtype":"error_during_execution","errors":["failed"],"is_error":true,"usage":{"input_tokens":5,"output_tokens":2}}), false).await;
    assert_eq!(output.stop_reason, StopReason::Error);
    assert_eq!(output.usage.total_tokens, 7);
    assert!(output.error_message.is_some());
}

#[tokio::test]
async fn resident_failure_bills_tokens_and_removes_closed_process() {
    let output = turn(serde_json::json!({"type":"result","subtype":"error_during_execution","errors":["failed"],"is_error":true,"usage":{"input_tokens":5,"output_tokens":2}}), true).await;
    assert_eq!(output.stop_reason, StopReason::Error);
    assert_eq!(output.usage.total_tokens, 7);
    assert_eq!(output.diagnostics.expect("continuity").len(),1);
}
