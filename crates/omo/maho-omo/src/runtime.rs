//! Composition-time runtime: the native `ComponentContext` plus the shared queue, logger and
//! deferred-macrotask scheduler (`omo-senpi/src/extension/compose.ts`, todo 47).

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use maho_ext_api::{
    ComponentLogger, DeferredMacrotask, ExtensionContext, FlagValue, IdleInjectionCoordinator, ToolDefinition,
};

use crate::capture::ToolCaptureRegistry;
use crate::coordinator::{IdleInjectionDelivery, IdleInjectionQueue};
use crate::logger::StderrLogger;
use crate::scheduler::{DeferredScheduler, TurnBarrier};

/// `compose.ts` batch window: `setTimeout(flush, 200)`.
pub const BATCH_WINDOW: Duration = Duration::from_millis(200);

pub type ConfigAccessor = Arc<dyn Fn(&str) -> Option<FlagValue> + Send + Sync>;

#[derive(Clone, Default)]
pub struct OmoRuntimeOptions {
    pub logger: Option<Arc<dyn ComponentLogger>>,
    pub flush_scheduler: Option<DeferredScheduler>,
    pub soon_scheduler: Option<DeferredScheduler>,
    pub defer_macrotask: Option<DeferredMacrotask>,
}

/// Upstream `ComponentContext` (`types.ts:47`): logger, flag accessor, captured tools, coordinator.
#[derive(Clone)]
pub struct OmoComponentContext {
    pub logger: Arc<dyn ComponentLogger>,
    pub config: ConfigAccessor,
    pub idle_coordinator: Arc<dyn IdleInjectionCoordinator>,
    pub defer_macrotask: DeferredMacrotask,
    pub get_captured_tools: Arc<dyn Fn() -> Vec<ToolDefinition> + Send + Sync>,
}

impl OmoComponentContext {
    pub fn get_flag(&self, name: &str) -> Option<FlagValue> {
        (self.config)(name)
    }

    /// Copies the shared runtime into the host's per-event `ExtensionContext`, which is where
    /// native components read `logger`, `idle_coordinator` and `defer_macrotask`.
    pub fn bind(&self, ctx: &mut ExtensionContext) {
        ctx.logger = Some(Arc::clone(&self.logger));
        ctx.idle_coordinator = Some(Arc::clone(&self.idle_coordinator));
        ctx.defer_macrotask = Some(Arc::clone(&self.defer_macrotask));
    }
}

#[derive(Clone)]
pub struct OmoRuntime {
    context: OmoComponentContext,
    coordinator: IdleInjectionQueue,
    capture: ToolCaptureRegistry,
    delivery: Arc<Mutex<IdleInjectionDelivery>>,
    config: Arc<Mutex<ConfigAccessor>>,
}

impl OmoRuntime {
    pub fn new(deliver: IdleInjectionDelivery, config: ConfigAccessor, options: OmoRuntimeOptions) -> Self {
        let logger = options.logger.unwrap_or_else(|| Arc::new(StderrLogger) as Arc<dyn ComponentLogger>);
        let barrier = TurnBarrier::new();
        let flush = options.flush_scheduler.unwrap_or_else(|| DeferredScheduler::runtime(Arc::clone(&barrier), Some(BATCH_WINDOW)));
        let barrier = flush.barrier();
        let soon = options.soon_scheduler.unwrap_or_else(|| DeferredScheduler::runtime(Arc::clone(&barrier), None));
        let defer_macrotask = options.defer_macrotask.unwrap_or_else(|| {
            let scheduler = DeferredScheduler::runtime(Arc::clone(&barrier), None);
            Arc::new(move |task| scheduler.schedule(task))
        });
        let delivery = Arc::new(Mutex::new(deliver));
        let bound_delivery = Arc::clone(&delivery);
        let coordinator = IdleInjectionQueue::new(Arc::new(move |message, mode| {
            let deliver = bound_delivery.lock().unwrap_or_else(PoisonError::into_inner).clone();
            deliver(message, mode)
        }), flush, soon);
        let config = Arc::new(Mutex::new(config));
        let bound_config = Arc::clone(&config);
        let capture = ToolCaptureRegistry::new();
        let captured = capture.clone();
        let context = OmoComponentContext {
            logger,
            config: Arc::new(move |name| {
                let read = bound_config.lock().unwrap_or_else(PoisonError::into_inner).clone();
                read(name)
            }),
            idle_coordinator: Arc::new(coordinator.clone()),
            defer_macrotask,
            get_captured_tools: Arc::new(move || captured.get_captured_tools()),
        };
        Self { context, coordinator, capture, delivery, config }
    }

    /// Rebinds a reload's extension API without replacing the queue or captured tool registry.
    pub fn rebind(&self, delivery: IdleInjectionDelivery, config: ConfigAccessor) {
        *self.delivery.lock().unwrap_or_else(PoisonError::into_inner) = delivery;
        *self.config.lock().unwrap_or_else(PoisonError::into_inner) = config;
        // A new generation reuses the retained queue (upstream builds a fresh coordinator on every
        // register), so re-arm it here; otherwise the previous generation's session-shutdown
        // retirement would refuse every injection for the rest of the process.
        self.coordinator.rearm();
    }

    pub fn context(&self) -> &OmoComponentContext {
        &self.context
    }

    pub fn coordinator(&self) -> IdleInjectionQueue {
        self.coordinator.clone()
    }

    /// Holds the turn barrier for a whole host turn, so no deferred pass runs inside it.
    pub fn enter_turn(&self) -> crate::scheduler::TurnGuard {
        self.coordinator.enter_turn()
    }

    pub fn logger(&self) -> Arc<dyn ComponentLogger> {
        Arc::clone(&self.context.logger)
    }

    pub fn defer_macrotask(&self) -> DeferredMacrotask {
        Arc::clone(&self.context.defer_macrotask)
    }

    pub fn bind(&self, ctx: &mut ExtensionContext) {
        self.context.bind(ctx);
    }

    pub fn capture_tools(&self, tools: Vec<ToolDefinition>) {
        self.capture.capture(tools);
    }

    pub fn captured_tools(&self) -> Vec<ToolDefinition> {
        self.capture.get_captured_tools()
    }
}
