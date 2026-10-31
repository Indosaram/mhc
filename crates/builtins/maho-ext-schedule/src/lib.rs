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
