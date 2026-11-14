//! Targeted undo of one monitor wake whose delivery receipt settled `Failed`.
//!
//! The wake was dequeued and its pacing token spent when it was taken, and the queue kept running
//! while its receipt was outstanding: new lines arrived, overflow grew, activity reset the streak.
//! A whole-snapshot restore (`MonitorDeliveryQueue::rollback`) would discard all of that, so this
//! undo is the literal inverse of what `take_ready_wake` did, recorded AT TAKE TIME, and it touches
//! nothing the wake did not:
//!
//! (a) EVENTS: the taken lines and the fingerprint-suppressed duplicates go back ahead of the id's
//!     current events in their original order, or at the id's original ordinal when it has none.
//!     There is no dedupe by content: identical repeated lines are normal monitor output.
//!     ACCEPTED: `notify` let new lines go inline assuming the taken lines were gone, so taken plus
//!     new can exceed `max_lines_per_injection`; `fit_wake` then degrades to a prefix wake, which is
//!     acceptable.
//! (b) OVERFLOW: only a WHOLE share deleted its entry, so only a whole share restores one, adding
//!     the deleted tally to whatever accrued since. A PARTIAL share kept its entry, which already
//!     incorporates the flushed lines, so it is left exactly as it is: adding the snapshot back
//!     would double-count the spilled lines in the notice.
//! (c) PACER: the spent token is returned arithmetically (`min(rate, tokens + 1)`), which commutes
//!     with the refills every read performs. A summary wake forgot its bucket instead of spending:
//!     there is nothing to return, so the retry starts from a FULL bucket - slightly more permissive
//!     than the old whole-snapshot rollback, intentionally and harmlessly.
//! (d) STAMPS: `last_injection`/`last_batch` go back to their pre-wake values only while they still
//!     hold what this wake wrote. A wake that wrote ABSENCE restores nothing, or a fingerprint that a
//!     `resume` deliberately cleared would be resurrected.
//! (e) STREAK: `consecutive_wakes`/`last_wake` rewind only if no activity was noted since the wake.
use crate::monitor_overflow::{MonitorOverflow, OverflowBook};
use crate::monitor_registry::MonitorEvent;
use crate::monitor_wake_pacing::WakePacer;
use std::collections::BTreeMap;

/// Events one wake removed for one monitor, with the index of that monitor's first event in the
/// queue at take time.
pub(crate) struct RemovedSlice {
    pub(crate) id: String,
    pub(crate) ordinal: usize,
    pub(crate) events: Vec<MonitorEvent>,
}

/// Everything one taken wake changed, recorded by `take_ready_wake` as it changed it.
pub(crate) struct WakeUndo {
    /// (a) Lines the wake carried, per monitor.
    pub(crate) taken: Vec<RemovedSlice>,
    /// (a) Lines dropped as a duplicate of the monitor's last batch.
    pub(crate) suppressed: Vec<RemovedSlice>,
    /// (b) Overflow entries a WHOLE share deleted, with their index, in deletion order.
    pub(crate) deleted_overflow: Vec<(usize, MonitorOverflow)>,
    /// (b) Monitors sent as a PARTIAL share: their overflow entry was retained and is not touched.
    pub(crate) retained_overflow: Vec<String>,
    /// (c) Monitors whose token was spent, with the rate it was spent at (summary wakes excluded).
    pub(crate) spent: Vec<(String, Option<f64>)>,
    /// (d) The `last_injection` value this wake wrote for every monitor in it.
    pub(crate) stamped_at: f64,
    /// (d) Each monitor's `last_injection` before the wake.
    pub(crate) last_injection: Vec<(String, Option<f64>)>,
    /// (d) Each monitor's `last_batch` before the wake, and what the wake wrote (`None`: removed).
    pub(crate) last_batch: Vec<(String, Option<String>, Option<String>)>,
    /// (e) The streak before the wake.
    pub(crate) consecutive_wakes: usize,
    pub(crate) last_wake: Option<f64>,
    /// (e) The queue's activity stamp when the wake was taken.
    pub(crate) activity: u64,
}

/// The queue state an undo writes to.
pub(crate) struct WakeUndoTarget<'a> {
    pub(crate) events: &'a mut Vec<MonitorEvent>,
    pub(crate) overflow: &'a mut OverflowBook,
    pub(crate) pacer: &'a mut WakePacer,
    pub(crate) last_injection: &'a mut BTreeMap<String, f64>,
    pub(crate) last_batch: &'a mut BTreeMap<String, String>,
    pub(crate) consecutive_wakes: &'a mut usize,
    pub(crate) last_wake: &'a mut Option<f64>,
    pub(crate) activity: u64,
}

fn event_id(event: &MonitorEvent) -> &str {
    match event {
        MonitorEvent::Line { id, .. } | MonitorEvent::Summary { id, .. } => id,
    }
}

/// `count - spilled` is computed by `OverflowBook::notice`; a restore that breaks this underflows.
fn assert_spill_tally(entry: &MonitorOverflow) {
    assert!(
        entry.spilled <= entry.count,
        "monitor {} overflow restored with spilled {} above count {}",
        entry.id,
        entry.spilled,
        entry.count
    );
}

/// WHOLE-share restore: everything in `current` arrived after the snapshot was deleted, so the
/// two tallies add up (a `record` on the new entry may itself have flushed, so its spills count).
fn merge_deleted(snapshot: MonitorOverflow, current: MonitorOverflow) -> MonitorOverflow {
    let mut kinds = snapshot.kinds;
    for kind in current.kinds {
        if !kinds.contains(&kind) {
            kinds.push(kind);
        }
    }
    let mut pending = snapshot.pending;
    pending.extend(current.pending);
    let (path, rotated) = if current.path.is_some() {
        (current.path, current.rotated)
    } else {
        (snapshot.path, snapshot.rotated)
    };
    MonitorOverflow {
        id: snapshot.id,
        description: snapshot.description,
        count: snapshot.count + current.count,
        kinds,
        human_input: snapshot.human_input || current.human_input,
        pending,
        spilled: snapshot.spilled + current.spilled,
        path,
        rotated,
    }
}

impl WakeUndo {
    pub(crate) fn apply(self, target: WakeUndoTarget<'_>) {
        // (a) Ascending original ordinal, so monitors land back in their queue order.
        let mut slices = self.taken;
        slices.extend(self.suppressed);
        slices.sort_by_key(|slice| slice.ordinal);
        for slice in slices {
            let at = target
                .events
                .iter()
                .position(|event| event_id(event) == slice.id)
                .unwrap_or_else(|| slice.ordinal.min(target.events.len()));
            let tail = target.events.split_off(at);
            target.events.extend(slice.events);
            target.events.extend(tail);
        }
        // (b) Reverse deletion order puts every entry back at the index it was deleted from.
        for (index, snapshot) in self.deleted_overflow.into_iter().rev() {
            let restored = match target.overflow.remove_full(&snapshot.id) {
                Some((_, current)) => merge_deleted(snapshot, current),
                None => snapshot,
            };
            assert_spill_tally(&restored);
            target.overflow.insert_at(index, restored);
        }
        for id in &self.retained_overflow {
            if let Some(entry) = target.overflow.get(id) {
                assert_spill_tally(entry);
            }
        }
        // (c)
        for (id, rate) in &self.spent {
            target.pacer.return_token(id, *rate);
        }
        // (d)
        for (id, before) in self.last_injection {
            if target.last_injection.get(&id) == Some(&self.stamped_at) {
                match before {
                    Some(at) => target.last_injection.insert(id, at),
                    None => target.last_injection.remove(&id),
                };
            }
        }
        for (id, before, written) in self.last_batch {
            let Some(written) = written else {
                continue;
            };
            if target.last_batch.get(&id) == Some(&written) {
                match before {
                    Some(batch) => target.last_batch.insert(id, batch),
                    None => target.last_batch.remove(&id),
                };
            }
        }
        // (e)
        if target.activity == self.activity {
            *target.consecutive_wakes = self.consecutive_wakes;
            *target.last_wake = self.last_wake;
        }
    }
}
