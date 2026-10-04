use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};

#[tokio::test]
async fn faux_registered_builtin_completes_headless_turn() {
    let script = load_script("hello").unwrap();
    let expected = script.responses[0].content.clone();
    let session = FauxSession::new(script).with_native_extension(NativeExtensionFactory {
        path: "<rule-activation>".into(), source_info: Default::default(),
        extension: Box::new(maho_ext_rule_activation::RuleActivation),
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.unwrap().unwrap();
    let messages = result["messages"].as_array().unwrap();
    let assistant = messages.iter().find(|message| message["role"] == "assistant").unwrap();
    let text = assistant["content"].as_array().unwrap().iter().filter_map(|block| block["text"].as_str()).collect::<Vec<_>>().join("");
    assert_eq!(text, expected);
    assert_eq!(result["scenario"], "hello");
}
