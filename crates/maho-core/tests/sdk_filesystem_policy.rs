use std::sync::Arc;

#[tokio::test]
async fn sdk_file_tools_consult_registered_extension_filesystem_policy() {
    let directory = tempfile::tempdir().expect("isolated SDK");
    let cwd = directory.path().to_string_lossy().into_owned();
    let denied_root = std::fs::canonicalize(directory.path()).expect("canonical cwd").join("denied");
    let policy_root = denied_root.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "sdk-filesystem-policy-fixture".into(), source_info: Default::default(),
        factory: Arc::new(move |api| {
            let root = policy_root.clone();
            Box::pin(async move {
                api.register_filesystem_policy(maho_ext_api::FilesystemPolicy {
                    check: Arc::new(move |request| {
                        let denied = request.canonical_path.starts_with(&root);
                        Box::pin(async move { Ok(if denied {
                            maho_ext_api::FilesystemPolicyDecision::Deny { reason: "fixture policy denied".into() }
                        } else { maho_ext_api::FilesystemPolicyDecision::Allow }) })
                    }),
                    denied_roots: None,
                });
                Ok(())
            })
        }),
    };
    let model = serde_json::from_value(serde_json::json!({
        "id":"fixture","name":"fixture","api":"faux","provider":"faux","baseUrl":"",
        "reasoning":false,"input":[],"contextWindow":128000,"maxTokens":4096,
        "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0}
    })).expect("model");
    let session = tokio::time::timeout(std::time::Duration::from_secs(5), maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(directory.path().join("agent").to_string_lossy().into_owned()),
        model: Some(model), session_manager: Some(maho_core::session_manager::SessionManager::in_memory(&cwd, None, None)),
        extension_factories: vec![factory], ..Default::default()
    })).await.expect("bounded SDK startup").expect("SDK session").session;
    // The denial surfaces as a tool error; only its filesystem effect is asserted here.
    let allowed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        drop(session.execute_tool("write", serde_json::json!({"path":"denied/secret.md","content":"x"}), Default::default()).await);
        session.execute_tool("write", serde_json::json!({"path":"allowed/open.md","content":"x"}), Default::default()).await
    }).await.expect("bounded tool execution");
    session.dispose().await;
    allowed.expect("policy-allowed write");
    assert!(!denied_root.join("secret.md").exists(), "policy-denied write must not reach the filesystem");
    assert_eq!(std::fs::read_to_string(directory.path().join("allowed/open.md")).expect("allowed write"), "x");
}
