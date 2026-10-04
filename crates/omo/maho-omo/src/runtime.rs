//! Composition-time runtime: the native `ComponentContext` plus the shared queue, logger and
//! deferred-macrotask scheduler (`omo-senpi/src/extension/compose.ts`, todo 47).

use std::sync::Arc;
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

pub struct OmoRuntime {
    context: OmoComponentContext,
    coordinator: IdleInjectionQueue,
    capture: ToolCaptureRegistry,
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
        let coordinator = IdleInjectionQueue::new(deliver, flush, soon);
        let capture = ToolCaptureRegistry::new();
        let captured = capture.clone();
        let context = OmoComponentContext {
            logger,
            config,
            idle_coordinator: Arc::new(coordinator.clone()),
            defer_macrotask,
            get_captured_tools: Arc::new(move || captured.get_captured_tools()),
        };
        Self { context, coordinator, capture }
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
