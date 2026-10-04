#![cfg(feature = "sdk")]

use maho_test_support::faux::{FauxResponse, FauxScript};
use maho_test_support::faux_session::FauxSession;

struct ShutdownProbe(std::sync::Arc<std::sync::atomic::AtomicBool>);

impl maho_ext_api::Extension for ShutdownProbe {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let observed = self.0.clone();
        api.on(maho_ext_api::EventKind::SessionShutdown, std::sync::Arc::new(move |_, _| {
            let observed = observed.clone();
            Box::pin(async move {
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
}

#[tokio::test]
async fn native_session_drives_prompt_reply_and_durable_entries() {
    let session = FauxSession::new(FauxScript {
        name: "native-hello".to_owned(),
        prompt: "hi".to_owned(),
        responses: vec![FauxResponse { content: "hello".to_owned(), stop_reason: "stop".to_owned() }],
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("native turn must settle").expect("native session");
    let messages = result["messages"].as_array().expect("messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"][0]["text"], "hi");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"][0]["text"], "hello");
    let entries = result["entries"].as_array().expect("entries");
    assert_eq!(entries.iter().filter(|entry| entry["type"] == "message").count(), 2);
    let events = result["events"].as_array().expect("events");
    assert_eq!(events.first().expect("start")["type"], "agent_start");
    assert_eq!(events.last().expect("end")["type"], "agent_end");
}

#[tokio::test]
async fn native_handle_drives_a_prompt_and_closes_cleanly() {
    let session = FauxSession::new(FauxScript {
        name: "native-handle".to_owned(),
        prompt: "hi".to_owned(),
        responses: vec![FauxResponse { content: "hello".to_owned(), stop_reason: "stop".to_owned() }],
    });
    let handle = session.run_native_handle().await.expect("native session");
    assert!(handle.cwd().is_dir());
    tokio::time::timeout(std::time::Duration::from_secs(10), handle.prompt("hi".to_owned()))
        .await.expect("prompt settles").expect("join").expect("prompt result");
    let messages = handle.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role(), "user");
    assert_eq!(messages[1].role(), "assistant");
    assert_eq!(handle.provider_calls().len(), 1);
    handle.close().await;
}

#[tokio::test]
async fn close_settles_a_held_prompt_and_observes_shutdown() {
    let shutdown = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let session = FauxSession::new(FauxScript {
        name: "native-close".to_owned(),
        prompt: "held".to_owned(),
        responses: vec![FauxResponse { content: "never".to_owned(), stop_reason: "stop".to_owned() }],
    })
    .with_native_extension(maho_ext_host::loader::NativeExtensionFactory {
        path: "<shutdown-probe>".to_owned(),
        source_info: maho_ext_api::SourceInfo::default(),
        extension: Box::new(ShutdownProbe(shutdown.clone())),
    });
    let handle = session.run_native_handle().await.expect("native session");
    let cwd = handle.cwd().to_path_buf();
    let gate = handle.hold_next_response();
    let prompt = handle.prompt("held".to_owned());
    tokio::time::timeout(std::time::Duration::from_secs(5), gate.entered())
        .await.expect("provider entered the held response");
    tokio::time::timeout(std::time::Duration::from_secs(5), handle.close())
        .await.expect("close settles a held prompt");
    let result = tokio::time::timeout(std::time::Duration::from_secs(5), prompt)
        .await.expect("prompt task joined").expect("join");
    assert!(result.is_ok());
    assert!(shutdown.load(std::sync::atomic::Ordering::SeqCst));
    assert!(!cwd.exists());
}
