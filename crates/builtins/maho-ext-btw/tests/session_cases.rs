use maho_ai::types::Message;
use maho_ext_api::SourceInfo;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::faux::{FauxResponse, FauxScript};
use maho_test_support::faux_session::{FauxSession, NativeSession};
use serde_json::Value;
use std::time::Duration;

fn response(content: &str) -> FauxResponse {
    FauxResponse { content: content.into(), stop_reason: "stop".into() }
}

fn script(responses: Vec<FauxResponse>) -> FauxScript {
    FauxScript { name: "btw-session".into(), prompt: "main".into(), responses }
}

fn btw_extension() -> NativeExtensionFactory {
    NativeExtensionFactory { path: "btw".into(), source_info: SourceInfo::default(), extension: Box::new(maho_ext_btw::Btw::default()) }
}

async fn boot(responses: Vec<FauxResponse>) -> NativeSession {
    FauxSession::new(script(responses)).with_native_extension(btw_extension()).run_native_handle().await.expect("native session")
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future).await.expect("bounded wait")
}

async fn finish(handle: tokio::task::JoinHandle<Result<(), String>>) {
    bounded(handle).await.expect("prompt task joined").expect("prompt result");
}

fn text_of(message: &Message) -> String {
    let value = serde_json::to_value(message).expect("message");
    match &value["content"] {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

fn user_texts(messages: &[Message]) -> Vec<String> {
    messages.iter().filter(|message| message.role() == "user").map(text_of).collect()
}

#[tokio::test]
async fn runs_a_side_query_in_parallel_with_an_in_flight_main_turn() {
    let session = boot(vec![response("main answer"), response("side answer")]).await;
    let gate = session.hold_next_response();
    let main = session.prompt("slow main question".into());
    bounded(gate.entered()).await.expect("main turn entered the held response");

    let side = session.prompt("/btw parallel question".into());
    finish(side).await;

    gate.release();
    finish(main).await;

    assert_eq!(session.provider_calls().len(), 2);
    let messages = session.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role(), "user");
    assert_eq!(messages[1].role(), "assistant");
    assert_eq!(text_of(&messages[1]), "main answer");
}

#[tokio::test]
async fn snapshots_context_synchronously_so_a_concurrent_turn_cannot_mix_generations() {
    let session = boot(vec![response("first answer"), response("side answer"), response("second answer")]).await;
    finish(session.prompt("first question".into())).await;

    let gate = session.hold_next_response();
    let side = session.prompt("/btw snapshot question".into());
    bounded(gate.entered()).await.expect("side query entered the held response");

    let side_call = &session.provider_calls()[1];
    assert_eq!(side_call.context.tools.as_ref().map_or(0, Vec::len), 0);
    let snapshot = user_texts(&side_call.context.messages);
    assert_eq!(snapshot, vec!["first question".to_owned(), "snapshot question".to_owned()]);

    let second = session.prompt("second question".into());
    gate.release();
    finish(side).await;
    finish(second).await;

    assert_eq!(user_texts(&session.provider_calls()[1].context.messages), snapshot);
    assert_eq!(session.provider_calls().len(), 3);
}
