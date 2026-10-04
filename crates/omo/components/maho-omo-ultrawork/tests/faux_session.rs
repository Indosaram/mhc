use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};

#[tokio::test]
async fn faux_registered_component_preserves_idle_prompt_and_provider_reply() {
    let script = load_script("hello").expect("shared faux script");
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference-hello.json"))
        .expect("generated pinned reference");
    let session = FauxSession::new(script).with_extension("omo").with_native_extension(
        NativeExtensionFactory {
            path: "<ultrawork>".into(),
            source_info: Default::default(),
            extension: Box::new(maho_omo_ultrawork::UltraworkComponent { arming: Default::default() }),
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

#[tokio::test]
async fn faux_trigger_matches_reference_hidden_delivery_and_typed_prompt() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference-trigger.json"))
        .expect("generated trigger reference");
    let mut script = load_script("hello").expect("shared faux responses");
    script.name = reference["scenario"].as_str().expect("scenario").into();
    let entries = reference["entries"].as_array().expect("reference entries");
    let user = entries.iter().find(|entry| entry["message"]["role"] == "user").expect("typed prompt");
    script.prompt = user["message"]["content"][0]["text"].as_str().expect("prompt text").into();
    let session = FauxSession::new(script).with_native_extension(NativeExtensionFactory {
        path: "<ultrawork>".into(), source_info: Default::default(),
        extension: Box::new(maho_omo_ultrawork::UltraworkComponent { arming: Default::default() }),
    });
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("bounded trigger session").expect("native trigger run");
    let custom_type = "omo-ultrawork:directive";
    let expected = entries.iter().find(|entry| entry["customType"] == custom_type).expect("reference injection");
    let messages = actual["messages"].as_array().expect("native messages");
    let custom = messages.iter()
        .find(|message| message["role"] == "custom" && message["customType"] == custom_type).expect("native injection");
    assert_eq!(custom["display"], expected["display"]);
    assert_eq!(custom["content"][0]["text"], expected["content"]);
    assert_eq!(messages.iter().filter(|message| message["customType"] == custom_type).count(), 1);
    assert_eq!(messages.iter().find(|message| message["role"] == "user").expect("native user")["content"], user["message"]["content"]);
}

#[tokio::test]
async fn faux_skill_family_suppression_matches_reference() {
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference-suppressed.json"))
        .expect("generated suppression reference");
    let entries = reference["entries"].as_array().expect("reference entries");
    assert!(!entries.iter().any(|entry| entry["customType"] == "omo-ultrawork:directive"));
    let user = entries.iter().find(|entry| entry["message"]["role"] == "user").expect("reference user");
    let mut script = load_script("hello").expect("shared faux responses");
    script.prompt = user["message"]["content"][0]["text"].as_str().expect("prompt").into();
    let session = FauxSession::new(script).with_native_extension(NativeExtensionFactory {
        path: "<ultrawork>".into(), source_info: Default::default(),
        extension: Box::new(maho_omo_ultrawork::UltraworkComponent { arming: Default::default() }),
    });
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("bounded suppression run").expect("native suppression run");
    let messages = actual["messages"].as_array().expect("messages");
    assert!(!messages.iter().any(|message| message["customType"] == "omo-ultrawork:directive"));
    assert_eq!(messages[0]["content"], user["message"]["content"]);
}
