use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use maho_ext_api::{DeliverAs, EventBus, EventKind, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, FlagValue, IdleInjection, IdleInjectionSource, LoadedExtension, SourceInfo, ToolDefinition};
use maho_omo::{DeferredScheduler, Delivery, IdleInjectionDelivery, IdleInjectionMessage, IdleInjectionQueue, TurnBarrier};
pub struct FakeComponent {
    register: Box<dyn Fn(&mut ExtensionApi) + Send + Sync>,
}

impl FakeComponent {
    pub fn new(register: impl Fn(&mut ExtensionApi) + Send + Sync + 'static) -> Self {
        Self { register: Box::new(register) }
    }
}

impl Extension for FakeComponent {
    fn register(&self, api: &mut ExtensionApi) {
        (self.register)(api);
    }
}

pub fn new_api() -> ExtensionApi {
    ExtensionApi::new(
        LoadedExtension::new("test", PathBuf::from("/repo"), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    )
}

pub fn flag_names(api: &ExtensionApi) -> Vec<String> {
    api.registered.flags.iter().map(|flag| flag.name.clone()).collect()
}

pub fn tool_names(api: &ExtensionApi) -> Vec<String> {
    api.registered.tools.iter().map(|tool| tool.definition.name.clone()).collect()
}

pub fn command_names(api: &ExtensionApi) -> Vec<String> {
    api.registered.commands.iter().map(|command| command.name.clone()).collect()
}

pub fn handler_events(api: &ExtensionApi) -> Vec<EventKind> {
    api.registered.handlers.keys().copied().collect()
}

pub fn fake_tool(name: &str) -> ToolDefinition {
    ToolDefinition::new(
        name,
        "test tool",
        serde_json::json!({ "type": "object" }),
        Arc::new(|_call| Box::pin(async { Ok(maho_ext_api::ToolResult::text("ok")) })),
    )
}

pub fn set_boolean_flag(api: &mut ExtensionApi, name: &str, value: bool) {
    api.set_flag(name, FlagValue::Boolean(value));
}

#[derive(Clone, Debug, PartialEq)]
pub struct Delivered {
    pub message: IdleInjectionMessage,
    pub deliver_as: DeliverAs,
}

#[derive(Clone, Default)]
pub struct DeliveryLog {
    calls: Arc<Mutex<Vec<Delivered>>>,
}

impl DeliveryLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn calls(&self) -> Vec<Delivered> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub fn delivery(&self) -> IdleInjectionDelivery {
        let calls = Arc::clone(&self.calls);
        Arc::new(move |message, deliver_as| {
            calls.lock().unwrap_or_else(PoisonError::into_inner).push(Delivered {
                message: message.clone(),
                deliver_as,
            });
            Delivery::Delivered
        })
    }
}

pub fn injection(key: &str, source: IdleInjectionSource, content: &str) -> IdleInjection {
    IdleInjection {
        key: key.to_owned(),
        source,
        custom_type: None,
        content: content.to_owned(),
        display: None,
        details: None,
        on_flushed: None,
        on_delivery_failed: None,
    }
}

/// A coordinator whose deferred flush is captured (upstream's manual `scheduleFlush`).
pub fn manual_coordinator() -> (IdleInjectionQueue, DeliveryLog, DeferredScheduler) {
    let log = DeliveryLog::new();
    let scheduler = DeferredScheduler::manual(TurnBarrier::new());
    let coordinator = IdleInjectionQueue::new(log.delivery(), scheduler.clone(), scheduler.clone());
    (coordinator, log, scheduler)
}

pub fn manual_runtime_options(logger: Arc<dyn maho_ext_api::ComponentLogger>) -> maho_omo::OmoRuntimeOptions {
    maho_omo::OmoRuntimeOptions {
        logger: Some(logger),
        flush_scheduler: Some(DeferredScheduler::manual(TurnBarrier::new())),
        soon_scheduler: Some(DeferredScheduler::manual(TurnBarrier::new())),
        defer_macrotask: None,
    }
}
