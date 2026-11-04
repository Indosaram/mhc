//! Registration shape and the no-history-transformation guarantee.

use maho_ext_api::{
    BeforeAgentStartEvent, BuildSystemPromptOptions, EventBus, EventKind, EventResult, Extension,
    ExtensionApi, ExtensionContext, ExtensionEvent, ExtensionRuntime, ExtensionSessionProfile,
    InputEvent, InputSource, LoadedExtension, SourceInfo,
};
use maho_ultrawork::{DISABLE_FLAG, UltraworkExtension};
use std::future::Future;
use std::task::{Context, Poll, Waker};

mod stub;

fn registered() -> ExtensionApi {
    let mut api = ExtensionApi::new(
        LoadedExtension::new("ultrawork", "/tmp".into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    UltraworkExtension::default().register(&mut api);
    api
}

#[test]
fn no_context_transform_handler_is_registered() {
    let api = registered();
    assert!(!api.registered.handlers.contains_key(&EventKind::Context));
    let mut kinds: Vec<_> = api.registered.handlers.keys().copied().collect();
    kinds.sort();
    let mut expected = vec![
        EventKind::Input,
        EventKind::BeforeAgentStart,
        EventKind::SessionCompact,
        EventKind::ToolResult,
    ];
    expected.sort();
    assert_eq!(kinds, expected);
}

#[test]
fn disable_flag_is_registered_as_boolean_defaulting_off() {
    let api = registered();
    assert!(
        api.registered
            .flags
            .iter()
            .any(|flag| flag.name == DISABLE_FLAG)
    );
    assert_eq!(
        api.get_flag(DISABLE_FLAG),
        Some(maho_ext_api::FlagValue::Boolean(false))
    );
}

/// Drives a future that never waits on anything external.
fn run<T>(future: impl Future<Output = T>) -> T {
    let mut future = std::pin::pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    match future.as_mut().poll(&mut cx) {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("handler future was not immediately ready"),
    }
}

#[test]
fn three_hundred_turns_deliver_one_directive_through_the_registered_handlers() {
    let api = registered();
    assert!(!api.registered.handlers.contains_key(&EventKind::Context));
    let ctx: ExtensionContext = stub::context("registration-300");
    let handlers = &api.registered.handlers;
    let mut directives = 0_usize;
    for turn in 0..300 {
        let text = if turn == 0 { "ulw go" } else { "continue" };
        let mut input = ExtensionEvent::Input(InputEvent {
            input_id: format!("i{turn}"),
            text: text.into(),
            images: None,
            source: InputSource::Interactive,
            streaming_behavior: None,
        });
        for handler in &handlers[&EventKind::Input] {
            run(handler(&mut input, &ctx)).expect("input handler");
        }
        let mut start = ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {
            prompt: text.into(),
            trigger: "prompt".into(),
            preview: false,
            images: None,
            system_prompt: String::new(),
            system_prompt_options: BuildSystemPromptOptions::default(),
        });
        for handler in &handlers[&EventKind::BeforeAgentStart] {
            if let EventResult::BeforeAgentStart(result) =
                run(handler(&mut start, &ctx)).expect("start handler")
                && result.message.is_some()
            {
                directives += 1;
            }
        }
    }
    assert_eq!(directives, 1);
    assert!(maho_ultrawork::snapshot("registration-300").armed);
}
