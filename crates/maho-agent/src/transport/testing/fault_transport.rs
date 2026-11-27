//! Port of senpi `packages/agent/src/transport/testing/fault-transport.ts`.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::durable::testing::manual_timers::ManualTimers;
use crate::transport::in_process_carrier::CarrierLink;
use crate::transport::transport::{CarrierConnection, CarrierMessage, CarrierSide};

use super::transport_harness::CarrierFault;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultProfileName {
    Slow,
    Lossy,
    Laggy,
    Flap,
    DropReattach,
}

impl FaultProfileName {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Slow => "slow",
            Self::Lossy => "lossy",
            Self::Laggy => "laggy",
            Self::Flap => "flap",
            Self::DropReattach => "drop-reattach",
        }
    }
}

pub const FAULT_PROFILE_NAMES: [FaultProfileName; 5] = [
    FaultProfileName::Slow,
    FaultProfileName::Lossy,
    FaultProfileName::Laggy,
    FaultProfileName::Flap,
    FaultProfileName::DropReattach,
];

#[derive(Debug, Clone, Default, Serialize)]
pub struct FaultStats {
    pub refused: usize,
    pub lost: usize,
    pub reordered: usize,
    pub drops: usize,
}

#[derive(Clone)]
struct Mulberry32 {
    state: Arc<Mutex<u32>>,
}

impl Mulberry32 {
    fn new(seed: u32) -> Self {
        Self {
            state: Arc::new(Mutex::new(seed)),
        }
    }

    fn next(&self) -> f64 {
        let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        *guard = guard.wrapping_add(0x6d2b_79f5);
        let mut t = *guard;
        t = (t ^ (t >> 15)).wrapping_mul(t | 1);
        t ^= t.wrapping_add((t ^ (t >> 7)).wrapping_mul(t | 61));
        f64::from(t ^ (t >> 14)) / 4_294_967_296.0
    }
}

type SendFn = Arc<
    dyn Fn(
            &Arc<dyn CarrierConnection>,
            &Arc<dyn CarrierSide>,
            CarrierMessage,
            Arc<AtomicBool>,
        ) -> bool
        + Send
        + Sync,
>;

struct FaultConnection {
    inner: Arc<dyn CarrierConnection>,
    side: Arc<dyn CarrierSide>,
    live: Arc<AtomicBool>,
    send_fn: SendFn,
}

impl CarrierConnection for FaultConnection {
    fn send(&self, message: CarrierMessage) -> bool {
        (self.send_fn)(&self.inner, &self.side, message, Arc::clone(&self.live))
    }

    fn close(&self) {
        self.inner.close();
    }
}

struct FaultWrappedSide {
    inner: Arc<dyn CarrierSide>,
    send_fn: SendFn,
    current_live: Mutex<Option<Arc<AtomicBool>>>,
}

impl CarrierSide for FaultWrappedSide {
    fn attached(&self, connection: Arc<dyn CarrierConnection>) {
        let live = Arc::new(AtomicBool::new(true));
        if let Some(prev) = self
            .current_live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .replace(Arc::clone(&live))
        {
            prev.store(false, Ordering::SeqCst);
        }
        self.inner.attached(Arc::new(FaultConnection {
            inner: connection,
            side: Arc::clone(&self.inner),
            live,
            send_fn: Arc::clone(&self.send_fn),
        }));
    }

    fn message(&self, message: &CarrierMessage) {
        self.inner.message(message);
    }

    fn writable(&self) {
        self.inner.writable();
    }

    fn detached(&self) {
        if let Some(prev) = self
            .current_live
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
        {
            prev.store(false, Ordering::SeqCst);
        }
        self.inner.detached();
    }
}

struct SlowState {
    tick: i64,
    count: usize,
    waiting: bool,
}

fn slow_wrap(
    timers: ManualTimers,
    stats: Arc<Mutex<FaultStats>>,
    side: Arc<dyn CarrierSide>,
) -> Arc<dyn CarrierSide> {
    let per_tick = 1usize;
    let tick_ms = 10i64;
    let state = Arc::new(Mutex::new(SlowState {
        tick: timers.now().div_euclid(tick_ms),
        count: 0,
        waiting: false,
    }));
    let send_fn: SendFn = Arc::new(move |inner, side_ref, message, live| {
        let now = timers.now();
        let tick = now.div_euclid(tick_ms);
        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
        if guard.tick != tick {
            guard.tick = tick;
            guard.count = 0;
        }
        if guard.count < per_tick {
            guard.count += 1;
            drop(guard);
            return inner.send(message);
        }
        stats.lock().unwrap_or_else(|p| p.into_inner()).refused += 1;
        if !guard.waiting {
            guard.waiting = true;
            let side_cb = Arc::clone(side_ref);
            let state_cb = Arc::clone(&state);
            let delay = (tick + 1) * tick_ms - now;
            let _ = timers.set_timer_sync(
                Box::new(move || {
                    state_cb.lock().unwrap_or_else(|p| p.into_inner()).waiting = false;
                    if live.load(Ordering::SeqCst) {
                        side_cb.writable();
                    }
                }),
                delay,
            );
        }
        false
    });
    Arc::new(FaultWrappedSide {
        inner: side,
        send_fn,
        current_live: Mutex::new(None),
    })
}

fn lossy_send(stats: Arc<Mutex<FaultStats>>) -> SendFn {
    let random = Mulberry32::new(7);
    let loss_rate = 0.2;
    Arc::new(move |inner, _, message, _| {
        if random.next() < loss_rate {
            stats.lock().unwrap_or_else(|p| p.into_inner()).lost += 1;
            return true;
        }
        inner.send(message)
    })
}

#[derive(Default)]
struct LaggyState {
    sent: u64,
    highest: u64,
}

fn laggy_wrap(
    timers: ManualTimers,
    stats: Arc<Mutex<FaultStats>>,
    random: Mulberry32,
    side: Arc<dyn CarrierSide>,
) -> Arc<dyn CarrierSide> {
    let latency_ms = 40i64;
    let jitter_ms = 60.0f64;
    let order = Arc::new(Mutex::new(LaggyState::default()));
    let send_fn: SendFn = Arc::new(move |inner, _, message, live| {
        let index = {
            let mut guard = order.lock().unwrap_or_else(|p| p.into_inner());
            guard.sent += 1;
            guard.sent
        };
        let delay = latency_ms + (random.next() * jitter_ms).floor() as i64;
        let inner_cb = Arc::clone(inner);
        let order_cb = Arc::clone(&order);
        let stats_cb = Arc::clone(&stats);
        let _ = timers.set_timer_sync(
            Box::new(move || {
                if !live.load(Ordering::SeqCst) {
                    return;
                }
                {
                    let mut guard = order_cb.lock().unwrap_or_else(|p| p.into_inner());
                    if index < guard.highest {
                        stats_cb.lock().unwrap_or_else(|p| p.into_inner()).reordered += 1;
                    }
                    if index > guard.highest {
                        guard.highest = index;
                    }
                }
                let _ = inner_cb.send(message);
            }),
            delay,
        );
        true
    });
    Arc::new(FaultWrappedSide {
        inner: side,
        send_fn,
        current_live: Mutex::new(None),
    })
}

fn run_flap(timers: &ManualTimers, stats: &Arc<Mutex<FaultStats>>, link: &CarrierLink) {
    let up_ms = 100i64;
    let down_ms = 60i64;
    let cycles = 8i64;
    let start_base = timers.now();
    for cycle in 0..cycles {
        let start = start_base + cycle * (up_ms + down_ms);
        let link_drop = link.clone();
        let stats_drop = Arc::clone(stats);
        let _ = timers.set_timer_sync(
            Box::new(move || {
                stats_drop.lock().unwrap_or_else(|p| p.into_inner()).drops += 1;
                link_drop.drop_link();
            }),
            start + up_ms - timers.now(),
        );
        let link_conn = link.clone();
        let _ = timers.set_timer_sync(
            Box::new(move || {
                link_conn.connect();
            }),
            start + up_ms + down_ms - timers.now(),
        );
    }
}

struct ProfileFault {
    name: FaultProfileName,
    timers: ManualTimers,
    stats: Arc<Mutex<FaultStats>>,
    link: Mutex<Option<CarrierLink>>,
    laggy_rng: Mulberry32,
    lossy_fn: SendFn,
    sent: AtomicU64,
}

impl CarrierFault for ProfileFault {
    fn wrap(&self, side: Arc<dyn CarrierSide>) -> Arc<dyn CarrierSide> {
        match self.name {
            FaultProfileName::Slow => slow_wrap(self.timers.clone(), Arc::clone(&self.stats), side),
            FaultProfileName::Lossy => Arc::new(FaultWrappedSide {
                inner: side,
                send_fn: Arc::clone(&self.lossy_fn),
                current_live: Mutex::new(None),
            }),
            FaultProfileName::Laggy => laggy_wrap(
                self.timers.clone(),
                Arc::clone(&self.stats),
                self.laggy_rng.clone(),
                side,
            ),
            FaultProfileName::Flap => Arc::new(FaultWrappedSide {
                inner: side,
                send_fn: Arc::new(|inner, _, msg, _| inner.send(msg)),
                current_live: Mutex::new(None),
            }),
            FaultProfileName::DropReattach => {
                let sent = Arc::new(AtomicU64::new(self.sent.load(Ordering::SeqCst)));
                let _ = sent;
                // Share counter across wrapped sides via link/stats.
                let stats = Arc::clone(&self.stats);
                let timers = self.timers.clone();
                let link_slot = Arc::new(Mutex::new(
                    self.link.lock().unwrap_or_else(|p| p.into_inner()).clone(),
                ));
                // Note: `wrap` is called before `start`, so share `self` via Arc or store link later.
                // Let's make DropReattach check a shared link_cell via a clean helper below.
                let _ = (stats, timers, link_slot);
                Arc::new(FaultWrappedSide {
                    inner: side,
                    send_fn: Arc::new(|inner, _, msg, _| inner.send(msg)),
                    current_live: Mutex::new(None),
                })
            }
        }
    }

    fn start(&self, link: &CarrierLink) {
        *self.link.lock().unwrap_or_else(|p| p.into_inner()) = Some(link.clone());
        if self.name == FaultProfileName::Flap {
            run_flap(&self.timers, &self.stats, link);
        }
    }
}

struct SharedDropReattachFault {
    timers: ManualTimers,
    stats: Arc<Mutex<FaultStats>>,
    link: Arc<Mutex<Option<CarrierLink>>>,
    sent: Arc<AtomicU64>,
}

impl CarrierFault for SharedDropReattachFault {
    fn wrap(&self, side: Arc<dyn CarrierSide>) -> Arc<dyn CarrierSide> {
        let timers = self.timers.clone();
        let stats = Arc::clone(&self.stats);
        let link_cell = Arc::clone(&self.link);
        let sent = Arc::clone(&self.sent);
        let send_fn: SendFn = Arc::new(move |inner, _, message, _| {
            let every_messages = 9u64;
            let reattach_ms = 30i64;
            let repetitions = 6usize;
            let count = sent.fetch_add(1, Ordering::SeqCst) + 1;
            if count.is_multiple_of(every_messages) {
                let target = link_cell.lock().unwrap_or_else(|p| p.into_inner()).clone();
                if let Some(target) = target {
                    let mut st = stats.lock().unwrap_or_else(|p| p.into_inner());
                    if st.drops < repetitions {
                        st.drops += 1;
                        drop(st);
                        let target_drop = target.clone();
                        let _ = timers.set_timer_sync(Box::new(move || target_drop.drop_link()), 0);
                        let _ =
                            timers.set_timer_sync(Box::new(move || target.connect()), reattach_ms);
                    }
                }
            }
            inner.send(message)
        });
        Arc::new(FaultWrappedSide {
            inner: side,
            send_fn,
            current_live: Mutex::new(None),
        })
    }

    fn start(&self, link: &CarrierLink) {
        *self.link.lock().unwrap_or_else(|p| p.into_inner()) = Some(link.clone());
    }
}

pub type FaultBuilder = Box<dyn FnOnce(&ManualTimers) -> Arc<dyn CarrierFault> + Send>;

pub fn create_carrier_fault(name: FaultProfileName, stats: Arc<Mutex<FaultStats>>) -> FaultBuilder {
    Box::new(move |timers| match name {
        FaultProfileName::DropReattach => Arc::new(SharedDropReattachFault {
            timers: timers.clone(),
            stats,
            link: Arc::new(Mutex::new(None)),
            sent: Arc::new(AtomicU64::new(0)),
        }),
        _ => Arc::new(ProfileFault {
            name,
            timers: timers.clone(),
            lossy_fn: lossy_send(Arc::clone(&stats)),
            stats,
            link: Mutex::new(None),
            laggy_rng: Mulberry32::new(11),
            sent: AtomicU64::new(0),
        }),
    })
}
