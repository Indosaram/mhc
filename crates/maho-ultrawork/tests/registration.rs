//! Registration shape and the no-history-transformation guarantee.

use maho_ext_api::{
    EventBus, EventKind, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile,
    InputSource, LoadedExtension, SourceInfo,
};
use maho_ultrawork::{DISABLE_FLAG, Engine, InputRequest, Sessions, UltraworkExtension};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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

/// Message stand-in whose clones are counted.
struct Counted(Arc<AtomicUsize>);

impl Clone for Counted {
    fn clone(&self) -> Self {
        self.0.fetch_add(1, Ordering::SeqCst);
        Self(Arc::clone(&self.0))
    }
}

#[test]
fn three_hundred_turns_clone_no_messages_and_run_no_context_handler() {
    let clones = Arc::new(AtomicUsize::new(0));
    let history: Vec<Counted> = (0..300).map(|_| Counted(Arc::clone(&clones))).collect();
    let api = registered();
    let context_handlers = api
        .registered
        .handlers
        .get(&EventKind::Context)
        .map_or(0, Vec::len);
    let engine = Engine::new(Arc::new(Sessions::default()));
    for (turn, _message) in history.iter().enumerate() {
        let text = if turn == 0 { "ulw go" } else { "continue" };
        engine.on_input(
            InputRequest {
                session: "s",
                text,
                source: InputSource::Interactive,
                streaming: false,
            },
            false,
        );
        engine.on_turn_start("s", false);
    }
    assert_eq!(context_handlers, 0);
    assert_eq!(clones.load(Ordering::SeqCst), 0);
    assert!(engine.sessions().snapshot("s").armed);
}
