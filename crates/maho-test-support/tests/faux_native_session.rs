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
async fn native_session_preserves_a_provider_refusal() {
    let session = FauxSession::new(FauxScript { name: "native-refusal".into(), prompt: "hi".into(),
        responses: vec![FauxResponse { content: "refused".into(), stop_reason: "error".into() }] });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await
        .expect("native turn settles").expect("native session result");
    assert_eq!(result["messages"].as_array().expect("messages").last().expect("final message")["stopReason"], "error");
}

#[tokio::test]
async fn native_session_runs_a_native_extension_before_prompt_completion() {
    let observed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let captured = observed.clone();
    let extension = maho_ext_host::loader::NativeExtensionFactory {
        path: "<session-start-probe>".into(), source_info: Default::default(),
        extension: Box::new(SessionStartProbe(captured)),
    };
    let session = FauxSession::new(FauxScript {
        name: "native-extension-fallback".into(), prompt: "hi".into(),
        responses: vec![FauxResponse { content: "native extension turn".into(), stop_reason: "stop".into() }],
    }).with_native_extension(extension);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await
        .expect("native turn settles").expect("native session result");
    assert!(observed.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(result["messages"].as_array().expect("messages").last().expect("assistant")["content"][0]["text"], "native extension turn");
}

struct SessionStartProbe(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl maho_ext_api::Extension for SessionStartProbe {
    fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
        let observed = self.0.clone();
        api.on(maho_ext_api::EventKind::SessionStart, std::sync::Arc::new(move |_, _| {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        }));
    }
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

#[tokio::test(flavor = "current_thread")]
async fn aborting_an_unpolled_prompt_does_not_leak_the_inflight_count() {
    let session = FauxSession::new(FauxScript {
        name: "native-abort".to_owned(),
        prompt: "x".to_owned(),
        responses: vec![FauxResponse { content: "never".to_owned(), stop_reason: "stop".to_owned() }],
    });
    let handle = session.run_native_handle().await.expect("native session");
    let cwd = handle.cwd().to_path_buf();
    let task = handle.prompt("x".to_owned());
    task.abort();
    let joined = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await.expect("cancelled task joins");
    assert!(joined.is_err());
    tokio::time::timeout(std::time::Duration::from_secs(5), handle.close())
        .await.expect("close does not hang after an unpolled abort");
    assert!(!cwd.exists());
}

#[tokio::test]
async fn native_session_runs_a_real_refusal_then_fallback_chain() {
    use std::sync::{Arc, Mutex};
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseFactory, faux_assistant_message};
    use maho_ai::types::StopReason;

    // The factory is model-aware: the primary model fails with a retryable transient error, the
    // fallback model answers. A single shared response queue cannot distinguish them, so this proves
    // the harness seam serves one factory step per provider call (primary + fallback).
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let factory: FauxResponseFactory = Arc::new(move |_context, _options, _state, model| {
        recorder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(model.id.clone());
        let message = if model.id == "faux-1" {
            faux_assistant_message("", FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Error), error_message: Some("overloaded".to_owned()),
                timestamp: Some(0), ..Default::default()
            })
        } else {
            faux_assistant_message("fallback answered", FauxAssistantMessageOptions {
                stop_reason: Some(StopReason::Stop), timestamp: Some(0), ..Default::default()
            })
        };
        Box::pin(async move { message })
    });
    // `maxRetries: 0` keeps the pure fallback path: the primary fails once, then the fallback rung.
    let session = FauxSession::new(FauxScript { name: "fallback".to_owned(), prompt: "hi".to_owned(), responses: Vec::new() })
        .with_settings(serde_json::json!({"retry": {"enabled": true, "maxRetries": 0, "modelFallback": true, "baseDelayMs": 0,
            "fallbackChains": {"faux/faux-1": ["faux/faux-2"]}}}))
        .with_faux_models(vec![
            FauxModelDefinition { id: "faux-1".to_owned(), name: Some("Faux 1".to_owned()), ..Default::default() },
            FauxModelDefinition { id: "faux-2".to_owned(), name: Some("Faux 2".to_owned()), ..Default::default() },
        ])
        .with_native_response_factory(factory);
    // The primary failure is a retryable transient error, so the core retry/fallback contract (not
    // the refusal path) is what routes it to the chain.
    assert!(maho_ai::utils::retry::is_retryable_error_message("overloaded"), "primary failure must be retryable");
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("native turn must settle").expect("native session");
    // Both models were actually requested: the primary, then the fallback rung.
    let models = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert!(models.contains(&"faux-1".to_owned()), "primary was not requested: {models:?}");
    assert!(models.contains(&"faux-2".to_owned()), "fallback was not requested: {models:?}");
    // The final assistant message is the fallback model's, from the faux provider.
    let messages = result["messages"].as_array().expect("messages");
    let last = messages.last().expect("last message");
    assert_eq!(last["role"], "assistant");
    assert_eq!(last["content"][0]["text"], "fallback answered");
    assert_eq!(last["model"], "faux-2");
    assert_eq!(last["provider"], "faux");
}

#[tokio::test]
async fn native_session_combines_a_native_extension_with_settings_fallback() {
    use std::sync::{Arc, Mutex};
    use std::sync::atomic::{AtomicBool, Ordering};
    use maho_ai::providers::faux::{FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseFactory, faux_assistant_message};
    use maho_ai::types::StopReason;

    struct FlagExtension(Arc<AtomicBool>);
    impl maho_ext_api::Extension for FlagExtension {
        fn register(&self, api: &mut maho_ext_api::ExtensionApi) {
            let flag = Arc::clone(&self.0);
            api.on(maho_ext_api::EventKind::SessionStart, Arc::new(move |_event, _context| {
                flag.store(true, Ordering::SeqCst);
                Box::pin(async { Ok::<maho_ext_api::EventResult, maho_ext_api::ExtensionFailure>(maho_ext_api::EventResult::None) })
            }));
        }
    }

    let extension_ran = Arc::new(AtomicBool::new(false));
    let native = maho_ext_host::loader::NativeExtensionFactory {
        path: "<test:flag>".to_owned(), source_info: Default::default(),
        extension: Box::new(FlagExtension(Arc::clone(&extension_ran))),
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let factory: FauxResponseFactory = Arc::new(move |_context, _options, _state, model| {
        recorder.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(model.id.clone());
        let message = if model.id == "faux-1" {
            faux_assistant_message("", FauxAssistantMessageOptions { stop_reason: Some(StopReason::Error),
                error_message: Some("overloaded".to_owned()), timestamp: Some(0), ..Default::default() })
        } else {
            faux_assistant_message("fallback answered", FauxAssistantMessageOptions { stop_reason: Some(StopReason::Stop), timestamp: Some(0), ..Default::default() })
        };
        Box::pin(async move { message })
    });
    let session = FauxSession::new(FauxScript { name: "ext-fallback".to_owned(), prompt: "hi".to_owned(), responses: Vec::new() })
        .with_native_extension(native)
        .with_settings(serde_json::json!({"retry": {"enabled": true, "maxRetries": 0, "modelFallback": true, "baseDelayMs": 0,
            "fallbackChains": {"faux/faux-1": ["faux/faux-2"]}}}))
        .with_faux_models(vec![
            FauxModelDefinition { id: "faux-1".to_owned(), name: Some("Faux 1".to_owned()), ..Default::default() },
            FauxModelDefinition { id: "faux-2".to_owned(), name: Some("Faux 2".to_owned()), ..Default::default() },
        ])
        .with_native_response_factory(factory);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("native turn must settle").expect("native session");
    assert!(extension_ran.load(Ordering::SeqCst), "native extension session_start must run");
    let models = seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    assert!(models.contains(&"faux-1".to_owned()) && models.contains(&"faux-2".to_owned()), "both models requested: {models:?}");
    let messages = result["messages"].as_array().expect("messages");
    assert_eq!(messages.last().and_then(|message| message["model"].as_str()), Some("faux-2"));
}
