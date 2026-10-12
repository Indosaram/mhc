use std::{
    collections::BTreeMap,
    os::unix::fs::PermissionsExt,
    sync::Arc,
};

use maho_ai::{
    auth::types::*,
    types::{
        ContentBlock, Context, Message, SimpleStreamOptions, StopReason, StreamKind, UserContent,
        UserMessage,
    },
};
use maho_ext_anthropic_subscription::{
    extension::AnthropicSubscriptionExtension, oauth_login::AnthropicSubscriptionOAuth,
};

struct Flow;

#[async_trait::async_trait]
impl OAuthAuth for Flow {
    fn name(&self) -> &str {
        "fixture"
    }

    async fn login(&self, _: &ProviderAuthInteraction) -> anyhow::Result<OAuthCredential> {
        panic!("no login")
    }

    async fn refresh(
        &self,
        _: &OAuthCredential,
        _: &maho_ai::utils::abort::AbortSignal,
    ) -> anyhow::Result<OAuthCredential> {
        panic!("ambient never refreshes")
    }

    async fn to_auth(&self, _: &OAuthCredential) -> anyhow::Result<ModelAuth> {
        Ok(ModelAuth::default())
    }
}

async fn turn_with_request_env(request: BTreeMap<String, String>) -> String {
    let directory = tempfile::tempdir().expect("directory");
    let executable = directory.path().join("claude");
    let observed = directory.path().join("observed.json");
    let script = format!(
        "#!/usr/bin/python3\nimport json,sys,os\nf=json.loads(sys.stdin.readline())\nprint(json.dumps({{'type':'control_response','response':{{'subtype':'success','request_id':f['request_id'],'response':{{}}}}}}),flush=True)\nf=json.loads(sys.stdin.readline())\nseen={{'token':os.environ.get('CLAUDE_CODE_OAUTH_TOKEN'),'nodeOptions':os.environ.get('NODE_OPTIONS'),'configDir':os.environ.get('CLAUDE_CONFIG_DIR'),'path':os.environ.get('PATH')}}\nopen({},'w').write(json.dumps(seen))\nif f.get('uuid'):\n print(json.dumps({{'type':'user','isReplay':True,'uuid':f['uuid']}}),flush=True)\nprint(json.dumps({{'type':'result','subtype':'success','result':'ok','user_message_uuid':f.get('uuid'),'usage':{{'input_tokens':1,'output_tokens':1}}}}),flush=True)\nfor line in sys.stdin: pass\n",
        serde_json::to_string(&observed.to_string_lossy().into_owned()).expect("literal"),
    );
    std::fs::write(&executable, script).expect("script");
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700))
        .expect("permissions");

    let model = maho_ai::models_generated::MODELS["anthropic"]
        .values()
        .next()
        .expect("model")
        .clone();
    let settings = Arc::new(|| {
        maho_ext_anthropic_subscription::settings::load(
            &serde_json::json!({}),
            &serde_json::Value::Null,
            &Default::default(),
        )
    });
    let oauth = Arc::new(AnthropicSubscriptionOAuth {
        store: Arc::new(maho_ai::auth::credential_store::InMemoryCredentialStore::new()),
        flow: Arc::new(Flow),
        settings,
        ambient: maho_ext_anthropic_subscription::availability::AmbientAuthStatusReader::new(
            Arc::new(|| Box::pin(async { Ok(false) })),
            Arc::new(|| 1),
            30_000,
        ),
    });
    let host = BTreeMap::from([("PATH".to_owned(), "/usr/bin".to_owned())]);
    let extension = AnthropicSubscriptionExtension::native(
        oauth,
        executable,
        None,
        directory.path().into(),
        directory.path().join("agent"),
        host,
    );
    let registry = extension.registry.clone();
    let mut options = SimpleStreamOptions::default();
    options.stream.session_id = Some("request-env-resident".into());
    options.stream.request.stream_kind = Some(StreamKind::Main);
    options.stream.request.env = Some(request);
    let stream = (extension.stream)(
        &model,
        &Context {
            messages: vec![Message::User(UserMessage {
                content: UserContent::Text("hello".into()),
                timestamp: 1,
            })],
            ..Default::default()
        },
        Some(options),
    );
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), stream.result())
        .await
        .expect("bounded native turn")
        .expect("output");
    assert_eq!(output.stop_reason, StopReason::Stop, "{:?}", output.error_message);
    assert!(matches!(&output.content[0], ContentBlock::Text(text) if text.text == "ok"));
    registry.lock().await.close_all().await.expect("cleanup");
    std::fs::read_to_string(&observed).expect("observed environment")
}

#[tokio::test]
async fn request_oauth_token_is_forwarded_and_subprocess_controls_are_dropped() {
    let request = BTreeMap::from([
        ("CLAUDE_CODE_OAUTH_TOKEN".to_owned(), "request-token".to_owned()),
        ("CLAUDE_CONFIG_DIR".to_owned(), "/tmp/request-config".to_owned()),
        (
            "NODE_OPTIONS".to_owned(),
            "--require=/tmp/request-hook.js".to_owned(),
        ),
        ("PATH".to_owned(), "/tmp/request-bin".to_owned()),
    ]);
    let observed: serde_json::Value =
        serde_json::from_str(&turn_with_request_env(request).await).expect("observed json");
    assert_eq!(observed["token"], "request-token");
    assert_eq!(
        observed["path"],
        "/usr/bin",
        "the host PATH must win over the request PATH"
    );
    assert_eq!(
        observed["configDir"],
        serde_json::Value::Null,
        "CLAUDE_CONFIG_DIR is not a request-forwarded variable"
    );
    assert_eq!(
        observed["nodeOptions"],
        serde_json::Value::Null,
        "NODE_OPTIONS is not a request-forwarded variable"
    );
}
