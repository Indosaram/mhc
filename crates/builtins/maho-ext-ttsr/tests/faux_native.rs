use maho_ai::providers::faux::{FauxAssistantMessageOptions,faux_assistant_message};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::FauxScript,faux_session::FauxSession};

fn session(prompt:&str)->FauxSession {
    FauxSession::new(FauxScript { name:"ttsr-native".into(),prompt:prompt.into(),responses:Vec::new() })
        .with_native_extension(NativeExtensionFactory { path:"<ttsr>".into(),source_info:Default::default(),extension:Box::new(maho_ext_ttsr::index::TtsrExtension) })
}
#[tokio::test]
async fn native_status_command_requires_no_provider_turn() {
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session("/ttsr").run_native()).await.unwrap().unwrap();
    assert_eq!(result["messages"],serde_json::json!([]));
}
#[tokio::test]
async fn native_clean_stream_survives_all_ttsr_hooks() {
    let session=session("answer").with_native_responses(vec![faux_assistant_message("ordinary answer",FauxAssistantMessageOptions { timestamp:Some(0),..Default::default() })]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let messages=result["messages"].as_array().unwrap();
    let assistant=messages.iter().find(|message|message["role"]=="assistant").unwrap();
    assert_eq!(assistant["content"][0]["text"],"ordinary answer"); assert_eq!(assistant["stopReason"],"stop");
    assert!(!result["entries"].as_array().unwrap().iter().any(|entry|entry["customType"]=="rule-activation"));
}
#[tokio::test]
async fn native_control_leak_records_activation_and_replaces_assistant() {
    let session=session("answer").with_native_responses(vec![faux_assistant_message("<|im_start|> <|im_start|> <|im_start|>",FauxAssistantMessageOptions { timestamp:Some(0),..Default::default() })]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    assert!(result["entries"].as_array().unwrap().iter().any(|entry|entry["customType"]=="rule-activation"&&entry["data"]["kind"]=="ttsr"));
    let messages=result["messages"].as_array().unwrap(); let assistant=messages.iter().find(|message|message["role"]=="assistant").unwrap();
    assert_eq!(assistant["stopReason"],"error");
    assert!(!assistant["content"].as_array().unwrap().iter().any(|block|block["text"].as_str().is_some_and(|text|text.contains("<|im_start|>"))));
}
