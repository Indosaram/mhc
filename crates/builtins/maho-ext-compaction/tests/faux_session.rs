use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};
use serde_json::{Value, json};

fn transcript(messages: impl Iterator<Item = Value>) -> Vec<Value> {
    messages.filter(|message| matches!(message["role"].as_str(), Some("user" | "assistant")))
        .map(|message| {
            let text = message["content"].as_str().map(str::to_owned).unwrap_or_else(|| {
                message["content"].as_array().into_iter().flatten()
                    .filter(|block| block["type"] == "text")
                    .filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("")
            });
            json!({"role": message["role"], "text": text})
        }).collect()
}

#[tokio::test]
async fn faux_registered_compaction_context_preserves_source_golden_transcript() {
    let reference: Value = serde_json::from_str(include_str!("golden-hello.json")).unwrap();
    let session = FauxSession::new(load_script("hello").unwrap())
        .with_native_extension(NativeExtensionFactory {
            path: "<compaction>".into(), source_info: Default::default(),
            extension: Box::new(maho_ext_compaction::CompactionExtension),
        });
    let actual = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.unwrap().unwrap();
    let expected = transcript(reference["entries"].as_array().unwrap().iter()
        .filter_map(|entry| entry.get("message").cloned()));
    assert_eq!(transcript(actual["messages"].as_array().unwrap().iter().cloned()), expected);
    assert_eq!(actual["scenario"], reference["scenario"]);
    assert!(!actual["entries"].as_array().unwrap().iter().any(|entry| entry["type"] == "compaction"));
}
