//! Port of senpi core/extensions/builtin/schedule (index.ts, tool.ts, live-delivery.ts).
//!
//! Durable scheduled prompts: the `schedule_prompt` tool writes job files; the open session delivers
//! its own due jobs (`live_delivery`), and a separate `schedule run` process fires the rest.
pub mod live_delivery;
pub mod tool;

use live_delivery::{LiveTimerPort, NodeLiveTimer, register_live_delivery};
use maho_core::schedule::native_probes::NowFn;
use maho_ext_api::{Extension, ExtensionApi};
use std::sync::Arc;

pub type TimerFactory = Arc<dyn Fn() -> Box<dyn LiveTimerPort> + Send + Sync>;

/// `ScheduleExtensionDeps`: injectable clock and timer.
#[derive(Clone, Default)]
pub struct ScheduleExtensionDeps {
    pub now: Option<NowFn>,
    pub timer: Option<TimerFactory>,
}

/// `createScheduleExtension({ now, timer })`.
#[derive(Clone, Default)]
pub struct Schedule {
    pub deps: ScheduleExtensionDeps,
}

pub fn create_schedule_extension(deps: ScheduleExtensionDeps) -> Schedule {
    Schedule { deps }
}

pub fn system_now() -> NowFn {
    Arc::new(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |time| {
                u64::try_from(time.as_millis()).unwrap_or(u64::MAX)
            })
    })
}

impl Extension for Schedule {
    fn register(&self, api: &mut ExtensionApi) {
        let now = self.deps.now.clone().unwrap_or_else(system_now);
        let timer = self.deps.timer.clone().unwrap_or_else(|| {
            Arc::new(|| Box::new(NodeLiveTimer::default()) as Box<dyn LiveTimerPort>)
        });
        let live_delivery = register_live_delivery(api, now.clone(), timer);
        tool::register_schedule_tool(api, now, live_delivery);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ext_api::{
        EventBus, EventKind, ExtensionRuntime, ExtensionSessionProfile, LoadedExtension, SourceInfo,
    };

    struct NoTimer;
    impl LiveTimerPort for NoTimer {
        fn arm(&self, _: u64, _: live_delivery::TimerFire) {}
        fn cancel(&self) {}
    }

    // senpi df2edde95a schedule/index.ts: the factory takes injected `now`/`timer` and wires live
    // delivery (session start/shutdown) together with the schedule_prompt tool.
    #[test]
    fn factory_wires_live_delivery_and_the_tool_with_injected_deps() {
        let built = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted = built.clone();
        let extension = create_schedule_extension(ScheduleExtensionDeps {
            now: Some(Arc::new(|| 42)),
            timer: Some(Arc::new(move || {
                counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Box::new(NoTimer) as Box<dyn LiveTimerPort>
            })),
        });
        let mut api = ExtensionApi::new(
            LoadedExtension::new("schedule", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        extension.register(&mut api);
        assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(), 1);
        assert_eq!(
            api.registered.handlers[&EventKind::SessionShutdown].len(),
            1
        );
        assert_eq!(
            api.registered
                .tools
                .iter()
                .map(|tool| tool.definition.name.as_str())
                .collect::<Vec<_>>(),
            [tool::SCHEDULE_PROMPT_TOOL]
        );
        // The timer is built per live session, never at registration.
        assert_eq!(built.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(Schedule::default().deps.now.is_none());
    }
}
