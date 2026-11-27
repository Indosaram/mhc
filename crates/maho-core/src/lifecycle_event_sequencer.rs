//! Port of senpi packages/coding-agent/src/core/lifecycle-event-sequencer.ts.
//!
//! Places a session's lifecycle events in its event stream by cause (senpi#4360, R10.c).

use std::collections::VecDeque;

pub struct LifecycleEventSequencer<Event> {
    held: Option<Vec<Event>>,
    queued: usize,
}

impl<Event> Default for LifecycleEventSequencer<Event> {
    fn default() -> Self {
        Self::new()
    }
}

pub type SequencerTask<Event> =
    Box<dyn FnOnce(&mut LifecycleEventSequencer<Event>, &mut dyn FnMut(Event)) + Send>;

impl<Event> LifecycleEventSequencer<Event> {
    pub fn new() -> Self {
        Self {
            held: None,
            queued: 0,
        }
    }

    pub fn emit(
        &mut self,
        event: Event,
        deliver: &mut dyn FnMut(Event),
        enqueue: &mut dyn FnMut(SequencerTask<Event>),
    ) where
        Event: Send + 'static,
    {
        if let Some(held) = self.held.as_mut() {
            held.push(event);
        } else if self.queued > 0 {
            self.queued += 1;
            enqueue(Box::new(move |seq, deliver| {
                seq.queued = seq.queued.saturating_sub(1);
                deliver(event);
            }));
        } else {
            deliver(event);
        }
    }

    /// Runs `handle` while holding emitted lifecycle events so they can be delivered immediately
    /// before the causing agent event on the ordered queue.
    pub fn hold<R>(&mut self, handle: impl FnOnce(&mut Self) -> R) -> (R, Vec<Event>) {
        let outer = self.held.take();
        self.held = Some(Vec::new());
        let result = handle(self);
        let held = self.held.take().unwrap_or_default();
        self.held = outer;
        self.queued += held.len();
        (result, held)
    }

    /// Delivers the events `hold` returned, from the session's queue.
    pub fn release(&mut self, held: Vec<Event>, deliver: &mut dyn FnMut(Event)) {
        self.queued = self.queued.saturating_sub(held.len());
        for event in held {
            deliver(event);
        }
    }

    pub fn queued_count(&self) -> usize {
        self.queued
    }
}

/// Helper queue for non-Send or local deterministic tests of `LifecycleEventSequencer`.
#[derive(Default)]
pub struct PendingSequencerQueue<Event> {
    tasks: VecDeque<SequencerTask<Event>>,
}

impl<Event> PendingSequencerQueue<Event> {
    pub fn push(&mut self, task: SequencerTask<Event>) {
        self.tasks.push_back(task);
    }

    pub fn drain(
        &mut self,
        seq: &mut LifecycleEventSequencer<Event>,
        deliver: &mut dyn FnMut(Event),
    ) {
        while let Some(task) = self.tasks.pop_front() {
            task(seq, deliver);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_events_precede_causing_event_and_direct_events_respect_queue_order() {
        let mut seq = LifecycleEventSequencer::<&'static str>::new();
        let mut log = Vec::new();
        let mut queue = PendingSequencerQueue::default();

        let ((), held) = seq.hold(|s| {
            s.emit("lc-held", &mut |e| log.push(e), &mut |t| queue.push(t));
        });
        assert!(log.is_empty());

        // Emit raised outside hold while held events sit on the queue joins the queue tail.
        seq.emit("lc-tail", &mut |e| log.push(e), &mut |t| queue.push(t));
        assert!(log.is_empty());

        // Simulate draining the ordered session queue: held lifecycle first, causing event, then tail.
        seq.release(held, &mut |e| log.push(e));
        log.push("agent-event");
        queue.drain(&mut seq, &mut |e| log.push(e));

        assert_eq!(log, vec!["lc-held", "agent-event", "lc-tail"]);
        assert_eq!(seq.queued_count(), 0);
    }
}
