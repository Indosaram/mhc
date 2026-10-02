use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, sync::Arc};
use maho_ai::{auth::types::*, types::{Context, Message, UserMessage, UserContent, StopReason, ContentBlock}};
use maho_ext_anthropic_subscription::{extension::AnthropicSubscriptionExtension, oauth_login::AnthropicSubscriptionOAuth};

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
    let settings = Arc::new(|| maho_ext_anthropic_subscription::settings::load(&serde_json::json!({}), &serde_json::Value::Null, &Default::default()));
    let oauth = Arc::new(AnthropicSubscriptionOAuth { store: Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new()), flow: Arc::new(Flow), settings, ambient: maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(Arc::new(|| Box::pin(async { Ok(false) })), Arc::new(|| 1), 30_000) });
    let extension = AnthropicSubscriptionExtension::native(oauth, executable, directory.path().into(), directory.path().join("agent"), BTreeMap::new());
    let registry = extension.registry.clone();
    let options = resident.then(|| {
        let mut options = maho_ai::types::SimpleStreamOptions::default();
        options.stream.session_id = Some("resident".into()); options.stream.request.stream_kind = Some(maho_ai::types::StreamKind::Main); options
    });
    let stream = (extension.stream)(&model, &Context { messages: vec![Message::User(UserMessage { content: UserContent::Text("hello".into()), timestamp: 1 })], ..Default::default() }, options);
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result()).await.expect("bounded native turn").expect("output");
    if resident && output.stop_reason == StopReason::Error { assert!(registry.lock().await.entries.is_empty()); }
    if resident && output.stop_reason == StopReason::Stop { assert_eq!(registry.lock().await.reapers.len(),1); }
    registry.lock().await.close_all().await.expect("cleanup"); assert!(registry.lock().await.reapers.is_empty()); output
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

#[tokio::test]
async fn native_factory_shares_resident_registry_and_cancels_reaper_on_close() {
    let output = turn(serde_json::json!({"type":"result","subtype":"success","result":"native","usage":{"input_tokens":3,"output_tokens":4},"stop_reason":"end_turn"}), true).await;
    assert_eq!(output.stop_reason,StopReason::Stop);
    assert_eq!(output.diagnostics.expect("continuity").len(),1);
}

#[tokio::test]
async fn managed_native_failure_rotates_only_before_visible_delta() {
    use maho_ext_anthropic_subscription::accounts::{AccountSlot,AccountSource,add_account,empty_credential};
    for visible in [false,true] {
        let directory=tempfile::tempdir().expect("directory");let executable=directory.path().join("claude");
        let attempts=directory.path().join("attempts");
        let script=format!("#!/usr/bin/python3\nimport json,sys,os\nf=json.loads(sys.stdin.readline())\nprint(json.dumps({{'type':'control_response','response':{{'subtype':'success','request_id':f['request_id'],'response':{{}}}}}}),flush=True)\nf=json.loads(sys.stdin.readline())\nslot=os.environ['CLAUDE_CODE_OAUTH_TOKEN']\nwith open({},'a') as log: log.write(slot+'\\n')\nif slot=='fixture-a':\n if {}:\n  print(json.dumps({{'type':'stream_event','event':{{'type':'content_block_start','index':0,'content_block':{{'type':'text','text':''}}}}}}),flush=True)\n  print(json.dumps({{'type':'stream_event','event':{{'type':'content_block_delta','index':0,'delta':{{'type':'text_delta','text':'partial'}}}}}}),flush=True)\n print(json.dumps({{'type':'result','subtype':'error_during_execution','errors':['rate_limit'],'is_error':True}}),flush=True)\nelse:\n print(json.dumps({{'type':'result','subtype':'success','result':'rotated','usage':{{}},'stop_reason':'end_turn'}}),flush=True)\nfor line in sys.stdin: pass\n",serde_json::to_string(attempts.to_str().expect("path")).expect("literal"),if visible {"True"}else {"False"});
        std::fs::write(&executable,script).expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
        let store=Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new());
        let mut credential=empty_credential();
        for name in ["a","b"] {credential=add_account(&credential,AccountSlot {name:name.into(),display_name:None,refresh:"fixture".into(),access:format!("fixture-{name}"),expires:9_000_000_000_000_000.0,source:AccountSource::Login,blocked_until:None,block_reason:None}).expect("slot");}
        credential.extra.insert("pinned".into(),serde_json::json!("a"));
        store.modify("anthropic-subscription",Box::new(move |_|Box::pin(async move {Ok(Some(Credential::OAuth(credential)))})),None).await.expect("seed");
        let settings=Arc::new(||maho_ext_anthropic_subscription::settings::load(&serde_json::json!({}),&serde_json::Value::Null,&Default::default()));
        let oauth=Arc::new(AnthropicSubscriptionOAuth {store:store.clone(),flow:Arc::new(Flow),settings,ambient:maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(Arc::new(||Box::pin(async {Ok(false)})),Arc::new(||1),30000)});
        let extension=AnthropicSubscriptionExtension::native(oauth,executable,directory.path().into(),directory.path().join("agent"),Default::default());
        let model=maho_ai::models_generated::MODELS["anthropic"].values().next().expect("model").clone();
        let stream=(extension.stream)(&model,&Context {messages:vec![Message::User(UserMessage {content:UserContent::Text("hello".into()),timestamp:1})],..Default::default()},None);
        let output=tokio::time::timeout(std::time::Duration::from_secs(10),stream.result()).await.expect("bounded turn").expect("output");
        assert_eq!(std::fs::read_to_string(attempts).expect("attempt log"),if visible {"fixture-a\n"}else {"fixture-a\nfixture-b\n"});
        assert_eq!(output.stop_reason,if visible {StopReason::Error}else {StopReason::Stop});
        if visible {assert!(output.error_message.expect("error").starts_with("senpi:no-turn-retry:"));}else {assert!(matches!(&output.content[0],ContentBlock::Text(text) if text.text=="rotated"));}
        let stored=store.read("anthropic-subscription",None).await.expect("read").expect("stored").into_oauth().expect("oauth");
        assert_eq!(stored.extra["accounts"][0]["blockReason"],"rate_limit");
    }
}
