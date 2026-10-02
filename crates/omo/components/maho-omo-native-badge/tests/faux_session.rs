use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};

#[tokio::test]
async fn faux_registered_component_preserves_idle_prompt_and_provider_reply() {
    let script = load_script("hello").expect("shared faux script");
    let prompt = script.prompt.clone();
    let response = script.responses[0].content.clone();
    let session = FauxSession::new(script).with_extension("omo").with_native_extension(
        NativeExtensionFactory {
            path: "<native-badge>".into(),
            source_info: Default::default(),
            extension: Box::new(maho_omo_native_badge::NativeBadgeComponent),
        },
    );
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("bounded native session").expect("registered component run");
    let messages = actual["messages"].as_array().expect("native messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], prompt);
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0]["text"], response);
    assert!(actual["events"].as_array().expect("session events").iter()
        .any(|event| event["type"] == "agent_end"));
}
