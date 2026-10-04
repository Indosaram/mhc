use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};

#[tokio::test]
async fn faux_registered_component_preserves_idle_prompt_and_provider_reply() {
    let script = load_script("hello").expect("shared faux script");
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference-hello.json"))
        .expect("generated pinned reference");
    let session = FauxSession::new(script).with_extension("omo").with_native_extension(
        NativeExtensionFactory {
            path: "<ulw-loop>".into(),
            source_info: Default::default(),
            extension: Box::new(maho_omo_ulw_loop::index::UlwLoopComponent { bin: None, js_runtime: "bun".into(), run_command: None, logger: None }),
        },
    );
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("bounded native session").expect("registered component run");
    let messages = actual["messages"].as_array().expect("native messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    let expected: Vec<_> = reference["entries"].as_array().expect("reference entries").iter()
        .filter_map(|entry| entry.get("message"))
        .filter(|message| matches!(message["role"].as_str(), Some("user" | "assistant")))
        .collect();
    assert_eq!(messages[0]["content"], expected[0]["content"]);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], expected[1]["content"]);
    assert!(actual["events"].as_array().expect("session events").iter()
        .any(|event| event["type"] == "agent_end"));
}
