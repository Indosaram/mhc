use std::sync::{Arc, Mutex};

#[tokio::test]
async fn native_sdk_account_facade_persists_mutations_in_shared_file_store() {
    let directory = tempfile::tempdir().expect("isolated SDK");
    let cwd = directory.path().to_string_lossy().into_owned();
    let agent_dir = directory.path().join("agent");
    let auth_path = agent_dir.join("auth.json");
    let storage = Arc::new(maho_core::auth_storage::AuthStorage::create(
        &auth_path.to_string_lossy(),
    ));
    storage
        .set(
            "sdk-account-fixture",
            Some(serde_json::json!({
                "type":"api_key","key":"fixture-flat-secret","accounts":[
                    {"name":"default","key":"fixture-first-secret","source":"login"},
                    {"name":"work","key":"fixture-second-secret","source":"import"}]
            })),
        )
        .expect("seed file store");
    let registry = Arc::new(Mutex::new(None));
    let captured = registry.clone();
    let events = Arc::new(Mutex::new(Vec::new()));
    let recorded = events.clone();
    let subscriptions = Arc::new(Mutex::new(Vec::new()));
    let retained = subscriptions.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "sdk-account-fixture".into(),
        source_info: Default::default(),
        factory: Arc::new(move |api| {
            let recorded = recorded.clone();
            retained.lock().expect("subscriptions").push(api.events.on(
                "provider-accounts-changed",
                Arc::new(move |event| recorded.lock().expect("events").push(event.clone())),
            ));
            let captured = captured.clone();
            api.on(
                maho_ext_api::EventKind::SessionStart,
                Arc::new(move |_, context| {
                    *captured.lock().expect("registry") = Some(context.model_registry.clone());
                    Box::pin(async { Ok(maho_ext_api::EventResult::None) })
                }),
            );
            Box::pin(async { Ok(()) })
        }),
    };
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    }))
    .expect("model");
    let session = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()),
        agent_dir: Some(agent_dir.to_string_lossy().into_owned()),
        auth_storage: Some(storage.clone()),
        model: Some(model),
        tools: Some(Vec::new()),
        extension_factories: vec![factory],
        session_manager: Some(maho_core::session_manager::SessionManager::in_memory(
            &cwd, None, None,
        )),
        ..Default::default()
    })
    .await
    .expect("native SDK")
    .session;
    let registry = registry
        .lock()
        .expect("registry")
        .take()
        .expect("registered context registry");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        let before = registry
            .get_credential_accounts("sdk-account-fixture")
            .await?;
        registry
            .pin_credential_account("sdk-account-fixture", Some("work"))
            .await?;
        registry
            .rename_credential_account("sdk-account-fixture", "work", Some("Work account"))
            .await?;
        let changed = registry
            .get_credential_accounts("sdk-account-fixture")
            .await?;
        registry
            .pin_credential_account("sdk-account-fixture", None)
            .await?;
        let missing = registry
            .remove_credential_account("sdk-account-fixture", "missing")
            .await;
        registry
            .remove_credential_account("sdk-account-fixture", "work")
            .await?;
        let after = registry
            .get_credential_accounts("sdk-account-fixture")
            .await?;
        Ok::<_, maho_ext_api::ExtensionFailure>((before, changed, after, missing))
    })
    .await;
    let shared_store = session.model_runtime().credentials.clone();
    session.dispose().await;
    let before_stale = storage.get("sdk-account-fixture");
    let stale = registry
        .rename_credential_account("sdk-account-fixture", "default", Some("Retired mutation"))
        .await;
    assert!(
        stale.is_err(),
        "disposed native registry must reject account mutation"
    );
    assert_eq!(storage.get("sdk-account-fixture"), before_stale);
    let (before, changed, after, missing) = result
        .expect("bounded operations")
        .expect("account mutations");
    assert!(Arc::ptr_eq(&storage, &shared_store));
    assert_eq!(before.len(), 2);
    assert!(changed[1].pinned);
    assert_eq!(changed[1].display_name.as_deref(), Some("Work account"));
    assert!(missing.is_err());
    assert_eq!(after.len(), 1);
    assert!(!after[0].pinned);
    let disk = maho_core::auth_storage::read_stored_credential(
        "sdk-account-fixture",
        &auth_path.to_string_lossy(),
    )
    .expect("persisted credential");
    assert_eq!(storage.get("sdk-account-fixture"), Some(disk.clone()));
    assert_eq!(disk["accounts"].as_array().expect("accounts").len(), 1);
    assert!(disk.get("pinned").is_none());
    assert_eq!(
        events.lock().expect("events").as_slice(),
        vec![serde_json::json!({"type":"accounts_changed","provider":"sdk-account-fixture"}); 4]
    );
    assert!(!format!("{before:?}{changed:?}{after:?}").contains("secret"));
}

/// omo 5.1.81 writes ChatGPT sign-ins as pooled slots with no flat tokens, beside claude CLI account slots.
/// The store must load that document whole and a session must authenticate from the slot.
#[tokio::test]
async fn an_omo_store_with_tokens_only_in_accounts_loads_and_authenticates() {
    let directory = tempfile::tempdir().expect("isolated agent dir");
    let agent_dir = directory.path().join("agent");
    std::fs::create_dir_all(&agent_dir).expect("agent dir");
    let auth_path = agent_dir.join("auth.json");
    let far = 4_102_444_800_000_u64;
    std::fs::write(
        &auth_path,
        serde_json::json!({
            "chatgpt-subscription": {
                "type": "oauth", "expires": far, "accountId": "fixture-account",
                "accounts": [
                    {"name": "login", "source": "login", "access": "fixture-access-1", "refresh": "fixture-refresh-1",
                     "expires": far, "route": "siwc", "issuer": "https://auth.openai.com", "subject": "fixture-subject-1",
                     "clientId": "fixture-client", "email": "one@example.invalid", "idToken": "fixture-id-token",
                     "scopes": ["openid", "chatgpt.tokens.use.direct"], "planUsageEnabled": true},
                    {"name": "login-2", "source": "login", "access": "fixture-access-2", "refresh": "fixture-refresh-2",
                     "expires": far, "route": "siwc", "issuer": "https://auth.openai.com", "subject": "fixture-subject-2",
                     "clientId": "fixture-client", "email": "two@example.invalid", "idToken": "fixture-id-token-2",
                     "scopes": ["openid", "chatgpt.tokens.use.direct"], "planUsageEnabled": true}
                ]
            },
            "anthropic-subscription": {
                "type": "oauth", "access": "anthropic-subscription-managed", "refresh": "anthropic-subscription-managed",
                "expires": far, "loginId": "fixture-login",
                "accounts": [{"name": "login", "source": "login", "store": "claude-cli", "dirId": "fixture-dir",
                              "signedInAt": 1, "profile": {"email": "c@example.invalid", "orgName": "o",
                              "subscriptionType": "max", "checkedAt": 1},
                              "check": {"model": "claude-haiku-5-5", "outcome": "ok", "at": 1}}]
            },
            "inferhub": {"type": "api_key", "key": "fixture-inferhub-key"}
        })
        .to_string(),
    )
    .expect("auth fixture");
    std::fs::write(
        agent_dir.join("settings.json"),
        r#"{"providerRiskConsent":{"chatgpt-subscription":"2026-01-01T00:00:00.000Z"}}"#,
    )
    .expect("consent fixture");
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            auth_path: Some(auth_path.clone()),
            agent_dir: Some(agent_dir.clone()),
            ..Default::default()
        },
    )
    .expect("model runtime");
    assert!(runtime.credentials.errors().is_empty());
    assert!(runtime.credentials.get("inferhub").is_some());
    assert!(runtime.credentials.get("anthropic-subscription").is_some());
    // A pooled entry without flat tokens resolves through one of its accounts, as a session's rotation selects it.
    let resolution = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        runtime.get_auth_with_overrides(
            "chatgpt-subscription",
            &maho_ai::models::AuthResolutionOverrides {
                slot_name: Some("login".into()),
                ..Default::default()
            },
        ),
    )
    .await
    .expect("bounded auth")
    .expect("auth resolves")
    .expect("chatgpt-subscription is signed in");
    let rendered = format!("{:?}{:?}", resolution.auth.api_key, resolution.auth.headers);
    assert!(rendered.contains("fixture-access-1"), "slot token is used");
}
