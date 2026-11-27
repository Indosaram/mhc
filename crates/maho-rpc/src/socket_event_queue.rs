//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-event-queue.ts`.
//!
//! One socket's FIFO: records charged by their bytes, barriers weighing nothing.

use crate::socket_baseline_barrier::BaselineBarrier;
use crate::socket_peer_recovery::ResyncReason;
use std::collections::VecDeque;

pub struct QueueEntry {
    pub line: String,
    pub bytes: usize,
    pub key: Option<String>,
    /// The record with its cumulative snapshot fields blanked (delta kept).
    pub demoted_line: Option<String>,
    /// Runs once with `true` when `line` reached the peer, with `false` when the actor dropped or
    /// replaced it unwritten.
    pub on_written: Option<Box<dyn FnOnce(bool) + Send>>,
    pub reply: bool,
    /// Set on a record captured behind a pending cut: reaches the peer only while held.
    pub held: Option<Box<dyn Fn() -> bool + Send>>,
    /// A targeted reply's destination check, repeated immediately before its write.
    pub current: Option<Box<dyn Fn() -> bool + Send>>,
}

pub struct AttachEntry {
    pub barrier: BaselineBarrier,
}

pub struct RecoveryEntry {
    pub id: u64,
    pub reason: ResyncReason,
    pub sessions: Vec<String>,
    pub barrier: BaselineBarrier,
    pub stale: bool,
}

pub enum QueueItem {
    Entry(QueueEntry),
    Attach(AttachEntry),
    Recovery(RecoveryEntry),
}

impl QueueItem {
    pub fn is_barrier(&self) -> bool {
        matches!(self, Self::Attach(_) | Self::Recovery(_))
    }

    pub fn is_reply(&self) -> bool {
        matches!(self, Self::Entry(e) if e.reply)
    }

    pub fn cancel_barrier(&mut self) {
        match self {
            Self::Attach(a) => a.barrier.cancel(),
            Self::Recovery(r) => r.barrier.cancel(),
            Self::Entry(_) => {}
        }
    }
}

/// One socket's FIFO: records charged by their bytes, barriers weighing nothing.
#[derive(Default)]
pub struct SocketEventQueue {
    items: VecDeque<QueueItem>,
    charged: usize,
}

impl SocketEventQueue {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bytes(&self) -> usize {
        self.charged
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn push(&mut self, item: QueueItem) {
        if let QueueItem::Entry(ref entry) = item {
            self.charged += entry.bytes;
        }
        self.items.push_back(item);
    }

    pub fn shift(&mut self) -> Option<QueueItem> {
        let item = self.items.pop_front()?;
        if let QueueItem::Entry(ref entry) = item {
            self.charged -= entry.bytes;
        }
        Some(item)
    }

    /// A record takes its place ahead of the first item matching `before` while queued, else the tail.
    pub fn insert_before<F>(&mut self, before: F, entry: QueueEntry)
    where
        F: Fn(&QueueItem) -> bool,
    {
        self.charged += entry.bytes;
        if let Some(pos) = self.items.iter().position(before) {
            self.items.insert(pos, QueueItem::Entry(entry));
        } else {
            self.items.push_back(QueueItem::Entry(entry));
        }
    }

    /// Mutate the recovery entry matching `id` in place (used when marking stale).
    pub fn recovery_mut(&mut self, id: u64) -> Option<&mut RecoveryEntry> {
        self.items.iter_mut().find_map(|item| match item {
            QueueItem::Recovery(r) if r.id == id => Some(r),
            _ => None,
        })
    }

    /// Lossless supersession of the queued record with `key`.
    pub fn supersede(&mut self, key: &str) {
        for item in &mut self.items {
            if let QueueItem::Entry(existing) = item
                && existing.key.as_deref() == Some(key)
            {
                if let Some(demoted) = existing.demoted_line.take() {
                    self.charged -= existing.bytes;
                    existing.bytes = demoted.len();
                    existing.line = demoted;
                    self.charged += existing.bytes;
                    if let Some(cb) = existing.on_written.take() {
                        cb(false);
                    }
                }
                existing.key = None;
                break;
            }
        }
    }

    /// Keeps the items `keep` accepts; every other record is released (`on_written(false)`) and
    /// every other barrier ended.
    pub fn retain<F>(&mut self, mut keep: F)
    where
        F: FnMut(&QueueItem) -> bool,
    {
        let old = std::mem::take(&mut self.items);
        let mut new_charged = 0usize;
        for mut item in old {
            if keep(&item) {
                if let QueueItem::Entry(ref e) = item {
                    new_charged += e.bytes;
                }
                self.items.push_back(item);
            } else {
                match &mut item {
                    QueueItem::Attach(a) => a.barrier.cancel(),
                    QueueItem::Recovery(r) => r.barrier.cancel(),
                    QueueItem::Entry(e) => {
                        if let Some(cb) = e.on_written.take() {
                            cb(false);
                        }
                    }
                }
            }
        }
        self.charged = new_charged;
    }

    /// Ends every barrier and empties the queue; the dropped records' callbacks never run.
    pub fn clear(&mut self) {
        for mut item in self.items.drain(..) {
            item.cancel_barrier();
        }
        self.charged = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn supersede_demotes_line_updates_bytes_and_fires_unwritten_callback() {
        let mut q = SocketEventQueue::new();
        let flag = Arc::new(AtomicBool::new(true));
        let f2 = flag.clone();
        q.push(QueueItem::Entry(QueueEntry {
            line: "1234567890".into(),
            bytes: 10,
            key: Some("k1".into()),
            demoted_line: Some("12".into()),
            on_written: Some(Box::new(move |w| f2.store(w, Ordering::SeqCst))),
            reply: false,
            held: None,
            current: None,
        }));
        assert_eq!(q.bytes(), 10);
        q.supersede("k1");
        assert_eq!(q.bytes(), 2);
        assert!(!flag.load(Ordering::SeqCst));
        let Some(QueueItem::Entry(e)) = q.shift() else {
            panic!("expected entry");
        };
        assert_eq!(e.line, "12");
        assert!(e.key.is_none());
        assert_eq!(q.bytes(), 0);
    }

    #[test]
    fn retain_drops_non_matching_entries_and_recomputes_bytes() {
        let mut q = SocketEventQueue::new();
        let dropped = Arc::new(AtomicBool::new(true));
        let d2 = dropped.clone();
        q.push(QueueItem::Entry(QueueEntry {
            line: "drop".into(),
            bytes: 4,
            key: None,
            demoted_line: None,
            on_written: Some(Box::new(move |w| d2.store(w, Ordering::SeqCst))),
            reply: false,
            held: None,
            current: None,
        }));
        q.push(QueueItem::Entry(QueueEntry {
            line: "keep".into(),
            bytes: 4,
            key: None,
            demoted_line: None,
            on_written: None,
            reply: true,
            held: None,
            current: None,
        }));
        q.retain(|item| item.is_reply());
        assert!(!dropped.load(Ordering::SeqCst));
        assert_eq!(q.len(), 1);
        assert_eq!(q.bytes(), 4);
    }
}
