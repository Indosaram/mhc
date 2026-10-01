//! Port of senpi `packages/agent/test/harness/execution-primitives.test.ts`.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_agent::harness::context::{
    BACKGROUND_CONTEXT, Context, create_context_key, get_telemetry_context, with_abort_signal, with_context_value,
    with_telemetry_context,
};
use maho_agent::harness::events::{HarnessEvent, HarnessEventBus, LaneQueuedItem};
use maho_agent::harness::execution::effect_gate::{AbortRequested, Cancellation, GateRefusal, create_gate};
use maho_agent::harness::hooks::{HookInvocation, HookName, HookRegistry, HookResult};
use maho_agent::harness::session::types::OperationKind;
use maho_agent::harness::telemetry::InMemoryTelemetryContext;
use maho_agent::harness::types::AgentHarnessStreamOptions;
use maho_agent::AgentMessage;
use maho_ai::types::{BoxFuture, Message, UserContent, UserMessage};
use serde_json::{Value, json};

fn user(content: &str, timestamp: i64) -> AgentMessage {
    AgentMessage::Llm(Message::User(UserMessage { content: UserContent::Text(content.to_owned()), timestamp }))
}

fn run_event(run_id: &str) -> HarnessEvent {
    HarnessEvent::run_start(run_id, 1, "main")
}

fn queue_item(entry_id: &str, content: &str, timestamp: i64) -> LaneQueuedItem {
    LaneQueuedItem {
        entry_id: entry_id.to_owned(),
        kind: "nextRun".to_owned(),
        item_type: "message".to_owned(),
        message: Some(user(content, timestamp)),
        custom_type: None,
        data: None,
    }
}

fn reporting(
    errors: Arc<std::sync::Mutex<Vec<String>>>,
) -> maho_agent::harness::hooks::HookErrorReporter {
    Arc::new(move |error, _hook, _lane, _context| {
        let errors = errors.clone();
        Box::pin(async move {
            errors.lock().unwrap().push(error);
        })
    })
}

fn handler(
    f: impl Fn(HookInvocation, Context) -> BoxFuture<'static, Result<HookResult, String>> + Send + Sync + 'static,
) -> maho_agent::harness::hooks::HookHandler {
    Arc::new(f)
}

async fn yield_now() {
    tokio::task::yield_now().await;
}

async fn settle() {
    for _ in 0..50 {
        yield_now().await;
    }
}

#[tokio::test]
async fn closes_starts_synchronously_and_signals_only_after_cancellation_commits() {
    let (gate, control) = create_gate();
    let cancellation = Cancellation::new();
    control.begin_abort(cancellation.clone());

    let refusal = gate.admit(|| ()).unwrap_err();
    let GateRefusal::Abort(abort) = refusal else {
        panic!("expected an abort refusal");
    };
    assert_eq!(abort, AbortRequested::new(cancellation.clone()));
    assert!(!abort.cancellation.is_resolved());
    assert!(!gate.signal().aborted());
    cancellation.resolve();
    cancellation.wait().await;
    control.signal_abort();
    assert!(gate.signal().aborted());
}

#[tokio::test]
async fn permanently_closes_and_signals_admitted_work() {
    let (gate, control) = create_gate();
    control.close("closed".to_owned());
    let refusal = gate.admit(|| ()).unwrap_err();
    assert_eq!(refusal.to_string(), "closed");
    assert!(gate.signal().aborted());
}

#[tokio::test]
async fn aggregates_before_run_messages_in_registration_order_with_each_handler_seeing_prior_output() {
    let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hooks = HookRegistry::new(reporting(errors.clone()));
    let seen = Arc::new(std::sync::Mutex::new(0usize));
    hooks.on(
        HookName::BeforeRun,
        handler(|_event, _context| {
            Box::pin(async { Ok(HookResult::BeforeRun { messages: vec![user("first", 2)] }) })
        }),
        None,
    );
    let second_seen = seen.clone();
    hooks.on(
        HookName::BeforeRun,
        handler(move |event, _context| {
            let second_seen = second_seen.clone();
            Box::pin(async move {
                *second_seen.lock().unwrap() = event.prompt.len();
                Ok(HookResult::BeforeRun { messages: vec![user("second", 3)] })
            })
        }),
        None,
    );

    let mut event = HookInvocation::new("main", "run");
    event.prompt = vec![user("prompt", 1)];
    let result = hooks.run_with_gate(HookName::BeforeRun, event, &create_gate().0, &BACKGROUND_CONTEXT).await.unwrap();
    let HookResult::BeforeRun { messages } = result else {
        panic!("expected before_run messages");
    };
    assert_eq!(messages.len(), 2);
    assert_eq!(*seen.lock().unwrap(), 2);
    assert!(errors.lock().unwrap().is_empty());
}

#[tokio::test]
async fn checks_the_effect_gate_immediately_before_the_complete_before_run_pipeline() {
    let hooks = Arc::new(HookRegistry::new(Arc::new(|_e, _h, _l, _c| Box::pin(async {}))));
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = Cancellation::new();
    let first = calls.clone();
    let first_release = release.clone();
    hooks.on(
        HookName::BeforeRun,
        handler(move |_event, _context| {
            let first = first.clone();
            let first_release = first_release.clone();
            Box::pin(async move {
                first.lock().unwrap().push("first:start".to_owned());
                first_release.wait().await;
                first.lock().unwrap().push("first:end".to_owned());
                Ok(HookResult::None)
            })
        }),
        None,
    );
    let second = calls.clone();
    hooks.on(
        HookName::BeforeRun,
        handler(move |_event, _context| {
            let second = second.clone();
            Box::pin(async move {
                second.lock().unwrap().push("second".to_owned());
                Ok(HookResult::None)
            })
        }),
        None,
    );

    let event = HookInvocation::new("main", "run");
    let closed_gate = create_gate();
    closed_gate.1.close("closed".to_owned());
    let refusal = hooks
        .run_with_gate(HookName::BeforeRun, event.clone(), &closed_gate.0, &BACKGROUND_CONTEXT)
        .await
        .unwrap_err();
    assert_eq!(refusal.to_string(), "closed");
    assert!(calls.lock().unwrap().is_empty());

    let running_gate = create_gate();
    let running = {
        let hooks = hooks.clone();
        let event = event.clone();
        let gate = running_gate.0.clone();
        async move { hooks.run_with_gate(HookName::BeforeRun, event, &gate, &BACKGROUND_CONTEXT).await }
    };
    let joined = tokio::spawn(running);
    settle().await;
    assert_eq!(&*calls.lock().unwrap(), &["first:start".to_owned()]);
    hooks.close("closed".to_owned());
    release.resolve();
    joined.await.unwrap().unwrap();
    assert_eq!(&*calls.lock().unwrap(), &["first:start".to_owned(), "first:end".to_owned(), "second".to_owned()]);
}

#[tokio::test]
async fn rejects_pre_aborted_invocations_and_combines_admitted_hook_cancellation_with_the_effect_gate() {
    let pre_aborted_hooks = HookRegistry::new(Arc::new(|_e, _h, _l, _c| Box::pin(async {})));
    let called = Arc::new(AtomicUsize::new(0));
    let called_clone = called.clone();
    pre_aborted_hooks.on(
        HookName::BeforeDrive,
        handler(move |_event, _context| {
            let called_clone = called_clone.clone();
            Box::pin(async move {
                called_clone.fetch_add(1, Ordering::SeqCst);
                Ok(HookResult::None)
            })
        }),
        None,
    );
    let mut event = HookInvocation::new("main", "run");
    event.operation = Some(OperationKind::Run);
    let pre_aborted_gate = create_gate();
    let controller = maho_ai::utils::abort::AbortController::new();
    controller.abort(Some(maho_ai::utils::abort::AbortReason::new("Error", "invocation aborted")));
    let context = with_abort_signal(controller.signal(), &BACKGROUND_CONTEXT);

    let refusal = pre_aborted_hooks
        .run_with_gate(HookName::BeforeDrive, event.clone(), &pre_aborted_gate.0, &context)
        .await
        .unwrap_err();
    assert_eq!(refusal.to_string(), "invocation aborted");
    assert_eq!(called.load(Ordering::SeqCst), 0);
    assert!(pre_aborted_gate.0.admit(|| ()).is_ok());

    let admitted_hooks = HookRegistry::new(Arc::new(|_e, _h, _l, _c| Box::pin(async {})));
    let admitted_gate = create_gate();
    let admitted_signal = Arc::new(std::sync::Mutex::new(None));
    let sink = admitted_signal.clone();
    admitted_hooks.on(
        HookName::BeforeDrive,
        handler(move |_event, context| {
            let sink = sink.clone();
            Box::pin(async move {
                *sink.lock().unwrap() = context.abort_signal();
                Ok(HookResult::None)
            })
        }),
        None,
    );
    admitted_hooks
        .run_with_gate(HookName::BeforeDrive, event, &admitted_gate.0, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert!(!admitted_signal.lock().unwrap().as_ref().unwrap().aborted());

    admitted_gate.1.close("gate closed".to_owned());
    let signal = admitted_signal.lock().unwrap().clone().unwrap();
    assert!(signal.aborted());
    assert_eq!(signal.reason().unwrap().message, "gate closed");
}

#[tokio::test]
async fn passes_tool_handlers_a_child_context_of_the_active_hook_span() {
    let telemetry = InMemoryTelemetryContext::new();
    const VALUE_KEY: maho_agent::harness::context::ContextKey<String> = create_context_key("hook.test.value");
    let hooks = HookRegistry::new(Arc::new(|_e, _h, _l, _c| Box::pin(async {})));
    let received = Arc::new(std::sync::Mutex::new(None));
    let sink = received.clone();
    hooks.on(
        HookName::BeforeTool,
        handler(move |_event, context| {
            let sink = sink.clone();
            Box::pin(async move {
                *sink.lock().unwrap() = context.value(&VALUE_KEY);
                get_telemetry_context(&context)
                    .start_span(
                        maho_agent::harness::telemetry::SpanOptions {
                            name: "handler.child".to_owned(),
                            attributes: serde_json::Map::new(),
                        },
                        |_span| Box::pin(async {}),
                    )
                    .await;
                Ok(HookResult::None)
            })
        }),
        None,
    );

    let invocation_span = telemetry.context();
    let context = with_context_value(
        &VALUE_KEY,
        "preserved".to_owned(),
        &with_telemetry_context(invocation_span, &BACKGROUND_CONTEXT),
    );
    let mut event = HookInvocation::new("main", "run");
    event.tool_call_id = Some("call".to_owned());
    event.tool_name = Some("tool".to_owned());
    hooks
        .run_tool_with_gate(HookName::BeforeTool, event, &create_gate().0, &context)
        .await
        .unwrap();

    assert_eq!(received.lock().unwrap().as_deref(), Some("preserved"));
    let spans = telemetry.get_spans();
    let handler_span = spans.iter().find(|span| span.name == "handler.child").expect("handler span");
    let hook_span = spans.iter().find(|span| span.name == "pi.harness.hook").expect("hook span");
    assert!(handler_span.parent_id == Some(hook_span.id));
}

#[tokio::test]
async fn treats_registration_ids_as_optional_metadata_and_fails_before_drive_closed() {
    let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hooks = HookRegistry::new(reporting(errors.clone()));
    let later = Arc::new(AtomicUsize::new(0));
    let later_clone = later.clone();
    hooks.on(
        HookName::BeforeDrive,
        handler(|_event, _context| Box::pin(async { Err("prerequisite failed".to_owned()) })),
        Some("duplicate".to_owned()),
    );
    hooks.on(
        HookName::BeforeDrive,
        handler(move |_event, _context| {
            let later_clone = later_clone.clone();
            Box::pin(async move {
                later_clone.fetch_add(1, Ordering::SeqCst);
                Ok(HookResult::None)
            })
        }),
        Some("duplicate".to_owned()),
    );

    let mut event = HookInvocation::new("main", "run");
    event.operation = Some(OperationKind::Run);
    let refusal = hooks
        .run_with_gate(HookName::BeforeDrive, event, &create_gate().0, &BACKGROUND_CONTEXT)
        .await
        .unwrap_err();
    assert_eq!(refusal.to_string(), "prerequisite failed");
    assert_eq!(later.load(Ordering::SeqCst), 0);
    assert_eq!(&*errors.lock().unwrap(), &["prerequisite failed".to_owned()]);
}

#[tokio::test]
async fn chains_transform_context_output_and_isolates_handler_failures() {
    let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hooks = HookRegistry::new(reporting(errors.clone()));
    let messages = vec![user("transformed", 2)];
    let first = messages.clone();
    hooks.on(
        HookName::TransformContext,
        handler(move |_event, _context| {
            let first = first.clone();
            Box::pin(async move {
                Ok(HookResult::TransformContext { messages: Some(first), system_prompt: Some("first".to_owned()) })
            })
        }),
        None,
    );
    hooks.on(
        HookName::TransformContext,
        handler(|event, _context| {
            Box::pin(async move {
                assert_eq!(event.system_prompt, "first");
                Err("ignored transform failure".to_owned())
            })
        }),
        None,
    );
    hooks.on(
        HookName::TransformContext,
        handler(|event, _context| {
            Box::pin(async move {
                assert_eq!(event.system_prompt, "first");
                Ok(HookResult::TransformContext { messages: None, system_prompt: Some("final".to_owned()) })
            })
        }),
        None,
    );

    let mut event = HookInvocation::new("main", "run");
    event.messages = vec![user("original", 1)];
    event.system_prompt = "base".to_owned();
    let result = hooks
        .run_with_gate(HookName::TransformContext, event, &create_gate().0, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let HookResult::TransformContext { messages: out, system_prompt } = result else {
        panic!("expected transform_context");
    };
    assert_eq!(out.unwrap().len(), 1);
    assert_eq!(system_prompt.as_deref(), Some("final"));
    assert_eq!(&*errors.lock().unwrap(), &["ignored transform failure".to_owned()]);
}

#[tokio::test]
async fn preserves_clear_all_before_request_patches_across_later_handlers() {
    let hooks = HookRegistry::new(Arc::new(|_e, _h, _l, _c| Box::pin(async {})));
    hooks.on(
        HookName::BeforeRequest,
        handler(|_event, _context| {
            Box::pin(async {
                Ok(HookResult::BeforeRequest {
                    stream_options: maho_agent::harness::types::AgentHarnessStreamOptionsPatch {
                        headers: Some(None),
                        metadata: Some(None),
                        ..Default::default()
                    },
                })
            })
        }),
        None,
    );
    let saw_cleared = Arc::new(AtomicUsize::new(0));
    let sink = saw_cleared.clone();
    hooks.on(
        HookName::BeforeRequest,
        handler(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                if event.stream_options.headers.is_none() && event.stream_options.metadata.is_none() {
                    sink.fetch_add(1, Ordering::SeqCst);
                }
                let mut headers = serde_json::Map::new();
                headers.insert("c".to_owned(), json!("3"));
                let mut metadata = serde_json::Map::new();
                metadata.insert("y".to_owned(), json!(2));
                Ok(HookResult::BeforeRequest {
                    stream_options: maho_agent::harness::types::AgentHarnessStreamOptionsPatch {
                        headers: Some(Some(
                            headers.into_iter().map(|(key, value)| (key, Some(value.as_str().unwrap().to_owned()))).collect(),
                        )),
                        metadata: Some(Some(metadata.into_iter().map(|(key, value)| (key, Some(value))).collect())),
                        ..Default::default()
                    },
                })
            })
        }),
        None,
    );

    let mut event = HookInvocation::new("main", "run");
    let mut headers = serde_json::Map::new();
    headers.insert("a".to_owned(), json!("1"));
    headers.insert("b".to_owned(), json!("2"));
    let mut metadata = serde_json::Map::new();
    metadata.insert("x".to_owned(), json!(1));
    event.stream_options = AgentHarnessStreamOptions {
        headers: Some(headers),
        metadata: Some(metadata),
        ..Default::default()
    };
    let result = hooks
        .run_with_gate(HookName::BeforeRequest, event, &create_gate().0, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    assert_eq!(saw_cleared.load(Ordering::SeqCst), 1);
    let HookResult::BeforeRequest { stream_options } = result else {
        panic!("expected before_request");
    };
    let headers = stream_options.headers.unwrap().unwrap();
    assert!(headers.get("a").unwrap().is_none());
    assert!(headers.get("b").unwrap().is_none());
    assert_eq!(headers.get("c").unwrap().as_deref(), Some("3"));
    let metadata = stream_options.metadata.unwrap().unwrap();
    assert!(metadata.get("x").unwrap().is_none());
    assert_eq!(metadata.get("y").unwrap().clone(), Some(json!(2)));
}

#[tokio::test]
async fn preserves_earlier_after_tool_fields_when_a_later_patch_returns_undefined() {
    let hooks = HookRegistry::new(Arc::new(|_e, _h, _l, _c| Box::pin(async {})));
    hooks.on(
        HookName::AfterTool,
        handler(|_event, _context| {
            Box::pin(async {
                Ok(HookResult::AfterTool(maho_agent::harness::hooks::AfterToolPatch {
                    content: Some(vec![maho_ai::types::ContentBlock::text("patched")]),
                    ..Default::default()
                }))
            })
        }),
        None,
    );
    hooks.on(
        HookName::AfterTool,
        handler(|_event, _context| {
            Box::pin(async {
                Ok(HookResult::AfterTool(maho_agent::harness::hooks::AfterToolPatch {
                    content: None,
                    is_error: Some(false),
                    ..Default::default()
                }))
            })
        }),
        None,
    );

    let mut event = HookInvocation::new("main", "run");
    event.content = vec![maho_ai::types::ContentBlock::text("raw")];
    event.is_error = true;
    let result = hooks
        .run_tool_with_gate(HookName::AfterTool, event, &create_gate().0, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let HookResult::AfterTool(patch) = result else {
        panic!("expected after_tool patch");
    };
    assert_eq!(patch.content.unwrap().len(), 1);
    assert_eq!(patch.is_error, Some(false));
}

#[tokio::test]
async fn accepts_explicit_false_structural_declines_and_rejects_true_conflicts() {
    let errors = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hooks = HookRegistry::new(reporting(errors.clone()));
    hooks.on(
        HookName::BeforeNavigation,
        handler(|_event, _context| {
            Box::pin(async {
                Ok(HookResult::Structural { decline: Some(true), value: Some(json!({ "summary": "ignored" })) })
            })
        }),
        None,
    );
    hooks.on(
        HookName::BeforeNavigation,
        handler(|_event, _context| {
            Box::pin(async {
                Ok(HookResult::Structural { decline: Some(false), value: Some(json!({ "summary": "selected" })) })
            })
        }),
        None,
    );

    let mut event = HookInvocation::new("main", "run");
    event.target_id = Some("target".to_owned());
    let result = hooks
        .run_with_gate(HookName::BeforeNavigation, event, &create_gate().0, &BACKGROUND_CONTEXT)
        .await
        .unwrap();
    let HookResult::Structural { decline, value } = result else {
        panic!("expected structural result");
    };
    assert_eq!(decline, Some(false));
    assert_eq!(value, Some(json!({ "summary": "selected" })));
    let errors = errors.lock().unwrap();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("cannot return both decline and summary"));
}

#[tokio::test]
async fn admits_the_complete_before_drive_pipeline_as_one_gated_effect() {
    let hooks = Arc::new(HookRegistry::new(Arc::new(|_e, _h, _l, _c| Box::pin(async {}))));
    let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
    let release = Cancellation::new();
    let first = calls.clone();
    let first_release = release.clone();
    hooks.on(
        HookName::BeforeDrive,
        handler(move |_event, _context| {
            let first = first.clone();
            let first_release = first_release.clone();
            Box::pin(async move {
                first.lock().unwrap().push("first:start".to_owned());
                first_release.wait().await;
                first.lock().unwrap().push("first:end".to_owned());
                Ok(HookResult::None)
            })
        }),
        None,
    );
    let second = calls.clone();
    hooks.on(
        HookName::BeforeDrive,
        handler(move |_event, _context| {
            let second = second.clone();
            Box::pin(async move {
                second.lock().unwrap().push("second".to_owned());
                Ok(HookResult::None)
            })
        }),
        None,
    );

    let mut event = HookInvocation::new("main", "run");
    event.operation = Some(OperationKind::Run);

    let abort_first_gate = create_gate();
    abort_first_gate.1.begin_abort(Cancellation::new());
    let refusal = hooks
        .run_with_gate(HookName::BeforeDrive, event.clone(), &abort_first_gate.0, &BACKGROUND_CONTEXT)
        .await
        .unwrap_err();
    assert!(matches!(refusal, maho_agent::harness::hooks::HookRunError::Gate(GateRefusal::Abort(_))));
    assert!(calls.lock().unwrap().is_empty());

    let start_first_gate = create_gate();
    let running = {
        let hooks = hooks.clone();
        let event = event.clone();
        let gate = start_first_gate.0.clone();
        async move { hooks.run_with_gate(HookName::BeforeDrive, event, &gate, &BACKGROUND_CONTEXT).await }
    };
    let joined = tokio::spawn(running);
    settle().await;
    assert_eq!(&*calls.lock().unwrap(), &["first:start".to_owned()]);
    start_first_gate.1.begin_abort(Cancellation::new());
    start_first_gate.1.signal_abort();
    release.resolve();
    joined.await.unwrap().unwrap();
    assert_eq!(&*calls.lock().unwrap(), &["first:start".to_owned(), "first:end".to_owned(), "second".to_owned()]);
}

#[tokio::test]
async fn buffers_between_snapshot_and_start_then_delivers_each_event_once_in_order() {
    let bus = HarnessEventBus::new();
    let watcher = bus.watch(
        json!({ "tipId": Value::Null }),
        Arc::new(|_event| true),
        None,
    );
    bus.emit(run_event("one"), BACKGROUND_CONTEXT.clone()).await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    watcher
        .start(Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(format!("{}:{}", event.event_type(), event.run_id().unwrap_or("")));
            })
        }))
        .await;
    bus.emit(run_event("two"), BACKGROUND_CONTEXT.clone()).await;
    settle().await;
    assert_eq!(watcher.snapshot(), json!({ "tipId": Value::Null }));
    assert_eq!(&*seen.lock().unwrap(), &["run_start:one".to_owned(), "run_start:two".to_owned()]);
    watcher.unsubscribe();
    bus.emit(run_event("three"), BACKGROUND_CONTEXT.clone()).await;
    settle().await;
    assert_eq!(seen.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn isolates_each_listener_from_payload_mutation() {
    let bus = HarnessEventBus::new();
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    bus.on(
        "config_update",
        Arc::new(|event, _context| {
            if let HarnessEvent {
                payload: maho_agent::harness::events::HarnessEventPayload::ConfigUpdate { value, .. },
                ..
            } = &event
            {
                let _ = value;
            }
            Box::pin(async {})
        }),
    );
    let sink = observed.clone();
    bus.on(
        "config_update",
        Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                if let maho_agent::harness::events::HarnessEventPayload::ConfigUpdate { value, .. } = &event.payload
                    && let Some(values) = value.as_array()
                {
                    sink.lock().unwrap().extend(values.iter().filter_map(|v| v.as_str().map(ToOwned::to_owned)));
                }
            })
        }),
    );
    bus.emit(
        HarnessEvent::config_update("activeTools", json!(["read"]), json!([]), Some("main".to_owned())),
        BACKGROUND_CONTEXT.clone(),
    )
    .await;
    assert_eq!(&*observed.lock().unwrap(), &["read".to_owned()]);
}

#[tokio::test]
async fn resolves_an_empty_batch_without_delivery() {
    let bus = HarnessEventBus::new();
    let calls = Arc::new(AtomicUsize::new(0));
    let sink = calls.clone();
    bus.on(
        "run_start",
        Arc::new(move |_event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.fetch_add(1, Ordering::SeqCst);
            })
        }),
    );
    bus.emit_batch(Vec::new(), BACKGROUND_CONTEXT.clone()).await;
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn isolates_listener_failures_and_emits_handler_error() {
    let bus = HarnessEventBus::new();
    let failures = Arc::new(std::sync::Mutex::new(Vec::new()));
    bus.on(
        "run_start",
        Arc::new(|_event, _context| {
            Box::pin(async {
                std::panic::panic_any("listener failed".to_owned());
            })
        }),
    );
    let sink = failures.clone();
    bus.on(
        "handler_error",
        Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                if let Some(message) = event.handler_error_message() {
                    sink.lock().unwrap().push(message.to_owned());
                }
            })
        }),
    );
    bus.emit(run_event("run"), BACKGROUND_CONTEXT.clone()).await;
    assert_eq!(&*failures.lock().unwrap(), &["listener failed".to_owned()]);
}

#[tokio::test]
async fn does_not_recurse_when_a_handler_error_listener_fails() {
    let bus = HarnessEventBus::new();
    let handler_errors = Arc::new(AtomicUsize::new(0));
    bus.on(
        "run_start",
        Arc::new(|_event, _context| Box::pin(async { std::panic::panic_any("listener failed".to_owned()) })),
    );
    let counter = handler_errors.clone();
    bus.on(
        "handler_error",
        Arc::new(move |_event, _context| {
            let counter = counter.clone();
            Box::pin(async move {
                counter.fetch_add(1, Ordering::SeqCst);
                std::panic::panic_any("error listener failed".to_owned());
            })
        }),
    );
    bus.emit(run_event("run"), BACKGROUND_CONTEXT.clone()).await;
    assert_eq!(handler_errors.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn keeps_concurrent_batches_contiguous_with_their_emitting_contexts() {
    let bus = HarnessEventBus::new();
    const SOURCE_KEY: maho_agent::harness::context::ContextKey<String> = create_context_key("event.batch.source");
    let first_context = with_context_value(&SOURCE_KEY, "first".to_owned(), &BACKGROUND_CONTEXT);
    let second_context = with_context_value(&SOURCE_KEY, "second".to_owned(), &BACKGROUND_CONTEXT);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    bus.on(
        "run_start",
        Arc::new(move |event, context| {
            let sink = sink.clone();
            Box::pin(async move {
                let source = context.value(&SOURCE_KEY).unwrap_or_default();
                sink.lock().unwrap().push((event.run_id().unwrap_or_default().to_owned(), source));
                tokio::task::yield_now().await;
            })
        }),
    );

    let first = {
        let bus = bus.clone();
        let context = first_context.clone();
        async move { bus.emit_batch(vec![run_event("a1"), run_event("a2")], context).await }
    };
    let second = {
        let bus = bus.clone();
        let context = second_context.clone();
        async move { bus.emit_batch(vec![run_event("b1"), run_event("b2")], context).await }
    };
    let (first, second) = tokio::join!(first, second);
    let _ = (first, second);
    let seen = seen.lock().unwrap().clone();
    assert_eq!(
        seen.iter().map(|(run_id, _)| run_id.clone()).collect::<Vec<_>>(),
        vec!["a1", "a2", "b1", "b2"]
    );
    assert!(seen[0].1 == "first" && seen[1].1 == "first");
    assert!(seen[2].1 == "second" && seen[3].1 == "second");
}

#[tokio::test]
async fn continues_watcher_delivery_after_listener_failure_and_reports_it() {
    let bus = HarnessEventBus::new();
    let failures = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = failures.clone();
    bus.on(
        "handler_error",
        Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                if let Some(message) = event.handler_error_message() {
                    sink.lock().unwrap().push(message.to_owned());
                }
            })
        }),
    );
    let watcher = bus.watch(json!({}), Arc::new(|event| event.event_type() == "run_start"), None);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    watcher
        .start(Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                let run_id = event.run_id().unwrap_or_default().to_owned();
                if run_id == "one" {
                    std::panic::panic_any("watcher failed".to_owned());
                }
                sink.lock().unwrap().push(run_id);
            })
        }))
        .await;
    bus.emit(run_event("one"), BACKGROUND_CONTEXT.clone()).await;
    bus.emit(run_event("two"), BACKGROUND_CONTEXT.clone()).await;
    settle().await;
    assert_eq!(&*seen.lock().unwrap(), &["two".to_owned()]);
    assert_eq!(&*failures.lock().unwrap(), &["watcher failed".to_owned()]);
}

#[tokio::test]
async fn serializes_concurrent_publications_in_process_order() {
    let bus = HarnessEventBus::new();
    let started = Cancellation::new();
    let release = Cancellation::new();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    let started_clone = started.clone();
    let release_clone = release.clone();
    bus.on(
        "run_start",
        Arc::new(move |event, _context| {
            let sink = sink.clone();
            let started = started_clone.clone();
            let release = release_clone.clone();
            Box::pin(async move {
                let run_id = event.run_id().unwrap_or_default().to_owned();
                sink.lock().unwrap().push(format!("{run_id}:start"));
                if run_id == "one" {
                    started.resolve();
                    release.wait().await;
                }
                sink.lock().unwrap().push(format!("{run_id}:end"));
            })
        }),
    );

    let one = {
        let bus = bus.clone();
        async move { bus.emit(run_event("one"), BACKGROUND_CONTEXT.clone()).await }
    };
    let one = tokio::spawn(one);
    started.wait().await;
    let two = {
        let bus = bus.clone();
        async move { bus.emit(run_event("two"), BACKGROUND_CONTEXT.clone()).await }
    };
    let two = tokio::spawn(two);
    settle().await;
    assert_eq!(&*seen.lock().unwrap(), &["one:start".to_owned()]);
    release.resolve();
    one.await.unwrap();
    two.await.unwrap();
    assert_eq!(
        &*seen.lock().unwrap(),
        &["one:start".to_owned(), "one:end".to_owned(), "two:start".to_owned(), "two:end".to_owned()]
    );
}

#[tokio::test]
async fn binds_listeners_and_watchers_when_a_batch_is_emitted() {
    let bus = HarnessEventBus::new();
    let started = Cancellation::new();
    let release = Cancellation::new();
    let started_clone = started.clone();
    let release_clone = release.clone();
    bus.on(
        "run_start",
        Arc::new(move |event, _context| {
            let started = started_clone.clone();
            let release = release_clone.clone();
            Box::pin(async move {
                if event.run_id() != Some("blocking") {
                    return;
                }
                started.resolve();
                release.wait().await;
            })
        }),
    );
    let blocking = {
        let bus = bus.clone();
        async move { bus.emit(run_event("blocking"), BACKGROUND_CONTEXT.clone()).await }
    };
    let blocking = tokio::spawn(blocking);
    started.wait().await;

    let queued = {
        let bus = bus.clone();
        let event = run_event("queued");
        let context = BACKGROUND_CONTEXT.clone();
        bus.begin_emit_batch(vec![event], context)
    };
    let queued = tokio::spawn(queued);
    let late_listener_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = late_listener_events.clone();
    bus.on(
        "run_start",
        Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(event.run_id().unwrap_or_default().to_owned());
            })
        }),
    );
    let late_watcher_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let watcher = bus.watch(json!({}), Arc::new(|event| event.event_type() == "run_start"), None);
    let sink = late_watcher_events.clone();
    watcher
        .start(Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(event.run_id().unwrap_or_default().to_owned());
            })
        }))
        .await;

    release.resolve();
    blocking.await.unwrap();
    queued.await.unwrap();
    assert!(late_listener_events.lock().unwrap().is_empty());
    assert!(late_watcher_events.lock().unwrap().is_empty());

    bus.emit(run_event("later"), BACKGROUND_CONTEXT.clone()).await;
    settle().await;
    assert_eq!(&*late_listener_events.lock().unwrap(), &["later".to_owned()]);
    assert_eq!(&*late_watcher_events.lock().unwrap(), &["later".to_owned()]);
}

#[tokio::test]
async fn drains_already_emitted_contiguous_batches_during_close_and_ignores_later_publication() {
    let bus = HarnessEventBus::new();
    let started = Cancellation::new();
    let release = Cancellation::new();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    let started_clone = started.clone();
    let release_clone = release.clone();
    bus.on(
        "run_start",
        Arc::new(move |event, _context| {
            let sink = sink.clone();
            let started = started_clone.clone();
            let release = release_clone.clone();
            Box::pin(async move {
                let run_id = event.run_id().unwrap_or_default().to_owned();
                sink.lock().unwrap().push(run_id.clone());
                if run_id == "blocking" {
                    started.resolve();
                    release.wait().await;
                }
            })
        }),
    );
    let blocking = {
        let bus = bus.clone();
        async move { bus.emit(run_event("blocking"), BACKGROUND_CONTEXT.clone()).await }
    };
    let blocking = tokio::spawn(blocking);
    started.wait().await;
    let batch = {
        let bus = bus.clone();
        async move { bus.emit_batch(vec![run_event("one"), run_event("two")], BACKGROUND_CONTEXT.clone()).await }
    };
    let batch = tokio::spawn(batch);
    settle().await;

    bus.close("closed");
    {
        let bus = bus.clone();
        async move { bus.emit(run_event("late"), BACKGROUND_CONTEXT.clone()).await }
    }
    .await;
    release.resolve();
    blocking.await.unwrap();
    batch.await.unwrap();
    assert_eq!(&*seen.lock().unwrap(), &["blocking".to_owned(), "one".to_owned(), "two".to_owned()]);
}

#[tokio::test]
async fn drops_pre_snapshot_delivery_and_holds_later_events_when_resnapshotting_inside_a_listener() {
    let bus = HarnessEventBus::new();
    let listener_started = Cancellation::new();
    let release_listener = Cancellation::new();
    let resnapshot_done = Cancellation::new();
    let watcher = bus.watch(
        json!({ "version": "old" }),
        Arc::new(|_event| true),
        Some(Arc::new(|_context, mark_boundary| {
            Box::pin(async move {
                mark_boundary();
                json!({ "version": "fresh" })
            })
        })),
    );
    let queue_events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = queue_events.clone();
    let started = listener_started.clone();
    let release = release_listener.clone();
    let done = resnapshot_done.clone();
    let watcher_for_listener = watcher.clone();
    watcher
        .start(Arc::new(move |event, context| {
            let sink = sink.clone();
            let started = started.clone();
            let release = release.clone();
            let done = done.clone();
            let watcher = watcher_for_listener.clone();
            Box::pin(async move {
                match event.event_type() {
                    "run_start" if event.run_id() == Some("blocking") => {
                        started.resolve();
                        release.wait().await;
                    }
                    "navigation_end" => {
                        let snapshot = watcher.resnapshot(context).await;
                        assert_eq!(snapshot, json!({ "version": "fresh" }));
                        done.resolve();
                    }
                    "queue_update" => {
                        let ids = event.queued_entry_ids();
                        sink.lock().unwrap().push(ids.first().cloned().unwrap_or_else(|| "empty".to_owned()));
                    }
                    _ => {}
                }
            })
        }))
        .await;

    bus.emit(run_event("blocking"), BACKGROUND_CONTEXT.clone()).await;
    listener_started.wait().await;
    let queued = {
        let bus = bus.clone();
        async move {
            bus.emit_batch(
                vec![
                    HarnessEvent::navigation_end("navigation", "completed", 2, "main"),
                    HarnessEvent::queue_update(vec![queue_item("stale", "stale", 2)], "main"),
                ],
                BACKGROUND_CONTEXT.clone(),
            )
            .await
        }
    };
    let queued = tokio::spawn(queued);
    release_listener.resolve();
    queued.await.unwrap();
    resnapshot_done.wait().await;
    settle().await;

    assert_eq!(watcher.snapshot(), json!({ "version": "fresh" }));
    assert!(queue_events.lock().unwrap().is_empty());
    bus.emit(
        HarnessEvent::queue_update(vec![queue_item("later", "later", 3)], "main"),
        BACKGROUND_CONTEXT.clone(),
    )
    .await;
    settle().await;
    assert_eq!(&*queue_events.lock().unwrap(), &["later".to_owned()]);
}

#[tokio::test]
async fn watch_from_snapshot_marks_boundaries_and_holds_events() {
    let bus = HarnessEventBus::new();
    let watcher = bus
        .watch_from_snapshot(
            Arc::new(|_context| Box::pin(async { json!({ "version": "captured" }) })),
            Arc::new(|_event| true),
            BACKGROUND_CONTEXT.clone(),
        )
        .await;
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    watcher
        .start(Arc::new(move |event, _context| {
            let sink = sink.clone();
            Box::pin(async move {
                sink.lock().unwrap().push(event.event_type().to_owned());
            })
        }))
        .await;
    bus.emit(run_event("run"), BACKGROUND_CONTEXT.clone()).await;
    settle().await;
    assert_eq!(watcher.snapshot(), json!({ "version": "captured" }));
    assert_eq!(&*seen.lock().unwrap(), &["run_start".to_owned()]);
}
