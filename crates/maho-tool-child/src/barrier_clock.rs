//! The tool child's barrier clock (senpi#3566, plan todo 4); port of `barrier-clock.ts`.
//!
//! One shared buffer of 32-bit cells (the `Int32Array` over a `SharedArrayBuffer` upstream), shared by the child's
//! protocol loop and every kernel worker. Index 0 is a monotonic clock; after it, one slot per in-flight launch holds
//! `{ackTick, invokeTick, invocations, inUse}`. The protocol loop stores `ackTick` when `launch_ack` arrives; every
//! launch path stores `invokeTick` and bumps `invocations` immediately before it calls the real spawn API.
//! `ackTick < invokeTick` read from the same clock is the causal proof that no command ran before the server recorded
//! its containment handle, independent of transport order or of how long the command runs. Tick 0 never occurs, so 0
//! in a slot means "not yet".
//!
//! `Atomics.wait` / `Atomics.notify` have no std equivalent, so the buffer carries a mutex + condvar pair that plays
//! the same role for `wait_for_ack` / `record_ack`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

const CLOCK: usize = 0;
const SLOT_FIELDS: usize = 4;
const ACK: usize = 0;
const INVOKE: usize = 1;
const INVOCATIONS: usize = 2;
const IN_USE: usize = 3;

pub const DEFAULT_LAUNCH_SLOTS: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotState {
    pub ack_tick: Option<u64>,
    pub invoke_tick: Option<u64>,
    pub invocations: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckWait {
    Acked,
    TimedOut,
}

impl AckWait {
    pub fn as_str(self) -> &'static str {
        match self {
            AckWait::Acked => "acked",
            AckWait::TimedOut => "timed-out",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BarrierClockError {
    #[error("barrier clock buffer has no slots")]
    NoSlots,
    #[error("barrier clock needs at least one slot, got {0}")]
    NeedsSlot(usize),
    #[error("no barrier slot {0}")]
    NoSlot(usize),
}

/// The shared cells (`SharedArrayBuffer`): handed to every thread that shares one clock.
#[derive(Debug)]
pub struct BarrierBuffer {
    cells: Vec<AtomicU32>,
    lock: Mutex<()>,
    wake: Condvar,
}

impl BarrierBuffer {
    /// A zeroed buffer of `1 + slots * SLOT_FIELDS` cells.
    pub fn with_slots(slots: usize) -> Self {
        Self::with_length(1 + slots * SLOT_FIELDS)
    }

    pub fn with_length(length: usize) -> Self {
        Self {
            cells: (0..length).map(|_| AtomicU32::new(0)).collect(),
            lock: Mutex::new(()),
            wake: Condvar::new(),
        }
    }

    pub fn length(&self) -> usize {
        self.cells.len()
    }
}

#[derive(Debug, Clone)]
pub struct BarrierClock {
    pub buffer: Arc<BarrierBuffer>,
    pub slots: usize,
}

impl BarrierClock {
    pub fn new(buffer: Arc<BarrierBuffer>) -> Result<Self, BarrierClockError> {
        let length = buffer.length();
        if length < 1 + SLOT_FIELDS || !(length - 1).is_multiple_of(SLOT_FIELDS) {
            return Err(BarrierClockError::NoSlots);
        }
        Ok(Self {
            slots: (length - 1) / SLOT_FIELDS,
            buffer,
        })
    }

    pub fn create(slots: usize) -> Result<Self, BarrierClockError> {
        if slots < 1 {
            return Err(BarrierClockError::NeedsSlot(slots));
        }
        Self::new(Arc::new(BarrierBuffer::with_slots(slots)))
    }

    fn cell(&self, index: usize) -> &AtomicU32 {
        // `index()` bounds every slot access and CLOCK is 0, so the cell exists; the buffer length is checked in `new`.
        &self.buffer.cells[index]
    }

    /// The next tick; strictly increasing across every thread sharing the buffer.
    pub fn tick(&self) -> u64 {
        u64::from(
            self.cell(CLOCK)
                .fetch_add(1, Ordering::SeqCst)
                .wrapping_add(1),
        )
    }

    /// Claims a free slot for one launch (protocol loop only), or `None` when all are in flight.
    pub fn claim_slot(&self) -> Option<usize> {
        for slot in 0..self.slots {
            let in_use = self.cell(1 + slot * SLOT_FIELDS + IN_USE);
            if in_use
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
            {
                continue;
            }
            for field in [ACK, INVOKE, INVOCATIONS] {
                self.cell(1 + slot * SLOT_FIELDS + field)
                    .store(0, Ordering::SeqCst);
            }
            return Some(slot);
        }
        None
    }

    pub fn release_slot(&self, slot: usize) -> Result<(), BarrierClockError> {
        self.cell(self.index(slot, IN_USE)?)
            .store(0, Ordering::SeqCst);
        Ok(())
    }

    /// Protocol loop, on `launch_ack`: records the tick and wakes a worker blocked in `wait_for_ack`.
    pub fn record_ack(&self, slot: usize) -> Result<u64, BarrierClockError> {
        let at = self.index(slot, ACK)?;
        let tick = self.tick();
        self.cell(at).store(tick as u32, Ordering::SeqCst);
        // Taking the lock orders the store before any waiter's re-check, so the wake-up cannot be lost.
        let _guard = self.buffer.lock.lock().unwrap_or_else(|p| p.into_inner());
        self.buffer.wake.notify_all();
        Ok(tick)
    }

    /// Launch path, immediately before the real call: stores the invoke tick and counts the invocation. Returns the
    /// ticks the proof carries; `ack_tick` is `None` when the call proceeds without an ack (only a mutation can do that).
    pub fn record_invoke(&self, slot: usize) -> Result<(Option<u64>, u64), BarrierClockError> {
        let ack_tick = u64::from(self.cell(self.index(slot, ACK)?).load(Ordering::SeqCst));
        let invoke_tick = self.tick();
        self.cell(self.index(slot, INVOKE)?)
            .store(invoke_tick as u32, Ordering::SeqCst);
        self.cell(self.index(slot, INVOCATIONS)?)
            .fetch_add(1, Ordering::SeqCst);
        Ok((
            if ack_tick == 0 { None } else { Some(ack_tick) },
            invoke_tick,
        ))
    }

    /// Worker thread only: blocks until the protocol loop records the slot's ack (`None` waits forever).
    pub fn wait_for_ack(
        &self,
        slot: usize,
        timeout: Option<Duration>,
    ) -> Result<AckWait, BarrierClockError> {
        let ack = self.cell(self.index(slot, ACK)?);
        let deadline = timeout.map(|limit| Instant::now() + limit);
        let mut guard = self.buffer.lock.lock().unwrap_or_else(|p| p.into_inner());
        while ack.load(Ordering::SeqCst) == 0 {
            match deadline {
                None => {
                    guard = self
                        .buffer
                        .wake
                        .wait(guard)
                        .unwrap_or_else(|p| p.into_inner());
                }
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        return Ok(AckWait::TimedOut);
                    }
                    guard = self
                        .buffer
                        .wake
                        .wait_timeout(guard, deadline - now)
                        .unwrap_or_else(|p| p.into_inner())
                        .0;
                }
            }
        }
        Ok(AckWait::Acked)
    }

    pub fn read(&self, slot: usize) -> Result<SlotState, BarrierClockError> {
        let ack_tick = u64::from(self.cell(self.index(slot, ACK)?).load(Ordering::SeqCst));
        let invoke_tick = u64::from(self.cell(self.index(slot, INVOKE)?).load(Ordering::SeqCst));
        Ok(SlotState {
            ack_tick: if ack_tick == 0 { None } else { Some(ack_tick) },
            invoke_tick: if invoke_tick == 0 {
                None
            } else {
                Some(invoke_tick)
            },
            invocations: u64::from(
                self.cell(self.index(slot, INVOCATIONS)?)
                    .load(Ordering::SeqCst),
            ),
        })
    }

    fn index(&self, slot: usize, field: usize) -> Result<usize, BarrierClockError> {
        if slot >= self.slots {
            return Err(BarrierClockError::NoSlot(slot));
        }
        Ok(1 + slot * SLOT_FIELDS + field)
    }
}

/// The proof the server enforces: an ack exists and it precedes the invocation on the shared clock.
pub fn proof_is_ordered(ack_tick: Option<u64>, invoke_tick: u64) -> bool {
    matches!(ack_tick, Some(ack) if ack < invoke_tick)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::mpsc;

    #[test]
    fn hands_out_strictly_increasing_unique_ticks_to_a_main_thread_and_a_worker_under_contention() {
        let clock = BarrierClock::create(1).unwrap();
        let worker_clock = clock.clone();
        let worker =
            std::thread::spawn(move || (0..5_000).map(|_| worker_clock.tick()).collect::<Vec<_>>());
        let from_main: Vec<u64> = (0..5_000).map(|_| clock.tick()).collect();
        let from_worker = worker.join().unwrap();
        for ticks in [&from_main, &from_worker] {
            for pair in ticks.windows(2) {
                assert!(pair[1] > pair[0]);
            }
        }
        let all: HashSet<u64> = from_main
            .iter()
            .chain(from_worker.iter())
            .copied()
            .collect();
        assert_eq!(all.len(), 10_000);
        assert_eq!(all.iter().min().copied(), Some(1));
        assert_eq!(all.iter().max().copied(), Some(10_000));
    }

    #[test]
    fn wakes_a_worker_blocked_on_the_ack_with_the_tick_the_protocol_loop_recorded() {
        let clock = BarrierClock::create(4).unwrap();
        let slot = clock.claim_slot().unwrap();
        let (ready_tx, ready_rx) = mpsc::channel();
        let (proof_tx, proof_rx) = mpsc::channel();
        let worker_clock = clock.clone();
        let worker = std::thread::spawn(move || {
            ready_tx.send(()).unwrap();
            assert_eq!(
                worker_clock.wait_for_ack(slot, None).unwrap(),
                AckWait::Acked
            );
            let (ack_tick, invoke_tick) = worker_clock.record_invoke(slot).unwrap();
            proof_tx.send((ack_tick, invoke_tick)).unwrap();
        });
        ready_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(
            clock.read(slot).unwrap(),
            SlotState {
                ack_tick: None,
                invoke_tick: None,
                invocations: 0
            }
        );
        let ack_tick = clock.record_ack(slot).unwrap();
        let (proof_ack, proof_invoke) = proof_rx.recv_timeout(Duration::from_secs(10)).unwrap();
        worker.join().unwrap();
        assert_eq!(proof_ack, Some(ack_tick));
        assert!(proof_is_ordered(proof_ack, proof_invoke));
        assert_eq!(
            clock.read(slot).unwrap(),
            SlotState {
                ack_tick: Some(ack_tick),
                invoke_tick: Some(proof_invoke),
                invocations: 1
            }
        );
    }

    #[test]
    fn reports_an_invocation_without_an_ack_as_an_unordered_proof_with_a_null_ack_tick() {
        let clock = BarrierClock::create(2).unwrap();
        let slot = clock.claim_slot().unwrap();
        let (ack_tick, invoke_tick) = clock.record_invoke(slot).unwrap();
        assert_eq!(ack_tick, None);
        assert!(!proof_is_ordered(ack_tick, invoke_tick));
        assert_eq!(clock.read(slot).unwrap().invocations, 1);
        assert!(!proof_is_ordered(Some(7), 7));
    }

    #[test]
    fn claims_and_recycles_slots_resetting_a_reused_slot_and_refuses_when_all_are_in_flight() {
        let clock = BarrierClock::create(2).unwrap();
        let first = clock.claim_slot().unwrap();
        let second = clock.claim_slot().unwrap();
        assert_eq!(
            [first, second].into_iter().collect::<HashSet<_>>(),
            HashSet::from([0, 1])
        );
        assert_eq!(clock.claim_slot(), None);
        clock.record_ack(first).unwrap();
        clock.record_invoke(first).unwrap();
        clock.release_slot(first).unwrap();
        assert_eq!(clock.claim_slot(), Some(first));
        assert_eq!(
            clock.read(first).unwrap(),
            SlotState {
                ack_tick: None,
                invoke_tick: None,
                invocations: 0
            }
        );
        assert_eq!(clock.read(2).unwrap_err().to_string(), "no barrier slot 2");
    }

    #[test]
    fn times_out_a_bounded_ack_wait_instead_of_blocking_forever() {
        let clock = BarrierClock::create(1).unwrap();
        let slot = clock.claim_slot().unwrap();
        let one_ms = Some(Duration::from_millis(1));
        assert_eq!(clock.wait_for_ack(slot, one_ms).unwrap(), AckWait::TimedOut);
        clock.record_ack(slot).unwrap();
        assert_eq!(clock.wait_for_ack(slot, one_ms).unwrap(), AckWait::Acked);
    }

    #[test]
    fn rejects_a_buffer_without_slots() {
        let error = BarrierClock::new(Arc::new(BarrierBuffer::with_length(3))).unwrap_err();
        assert_eq!(error.to_string(), "barrier clock buffer has no slots");
        assert_eq!(
            BarrierClock::create(0).unwrap_err().to_string(),
            "barrier clock needs at least one slot, got 0"
        );
    }
}
