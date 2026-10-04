use maho_ext_api::SourceInfo;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::faux::{FauxResponse, FauxScript};
use maho_test_support::faux_session::{FauxSession, NativeSession};
use serde_json::json;

fn script() -> FauxScript {
    FauxScript {
        name: "btw-session".into(),
        prompt: "main".into(),
        responses: vec![FauxResponse { content: "main answer".into(), stop_reason: "stop".into() }, FauxResponse { content: "side answer".into(), stop_reason: "stop".into() }],
    }
}

fn btw_extension() -> NativeExtensionFactory {
    NativeExtensionFactory { path: "btw".into(), source_info: SourceInfo::default(), extension: Box::new(maho_ext_btw::Btw::default()) }
}

async fn boot() -> NativeSession {
    FauxSession::new(script()).with_native_extension(btw_extension()).run_native_handle().await.expect("native session")
}

fn text_of(message: &serde_json::Value) -> String {
    match &message["content"] {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts.iter().filter_map(|part| part["text"].as_str()).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

#[tokio::test]
async fn runs_a_side_query_in_parallel_with_an_in_flight_main_turn() {
    let session = boot().await;
    let gate = session.hold_next_response();
    let main = session.prompt("slow main question".into());
    gate.entered().await.expect("main turn entered the held response");

    let side = session.prompt("/btw parallel question".into());
    side.await.expect("side query completes while the main turn is held");

    gate.release();
    main.await.expect("main turn");

    assert_eq!(session.provider_calls().len(), 2);
    let messages = session.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(text_of(&messages[1]), "main answer");
}

#[tokio::test]
async fn snapshots_context_synchronously_so_a_concurrent_turn_cannot_mix_generations() {
    let session = boot().await;
    session.prompt("first question".into()).await.expect("first");

    let gate = session.hold_next_response();
    let side = session.prompt("/btw snapshot question".into());
    gate.entered().await.expect("side query entered the held response");
    let second = session.prompt("second question".into());
    gate.release();
    side.await.expect("side query");
    second.await.expect("second turn");

    let side_call = &session.provider_calls()[1];
    let user_texts = side_call
        .context
        .messages
        .iter()
        .filter(|message| message["role"] == "user")
        .map(text_of)
        .collect::<Vec<_>>();
    assert_eq!(user_texts, vec!["first question".to_owned(), "snapshot question".to_owned()]);
    assert_eq!(json!(side_call.context.tools), json!([]));
}
