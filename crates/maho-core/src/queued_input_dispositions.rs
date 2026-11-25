//! Port of senpi packages/coding-agent/src/core/queued-input-dispositions.ts.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueuedDisposition {
    Queued,
    Started,
}

impl QueuedDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Started => "started",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    QueuedPending,
    Queued,
    Delivered,
}

struct Tracked {
    input_id: String,
    phase: Phase,
}

pub type DispositionEmitFn = Arc<
    dyn Fn(String, QueuedDisposition) -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync,
>;

pub struct QueuedInputDispositions {
    by_message: Mutex<HashMap<String, Arc<Mutex<Tracked>>>>,
    awaiting_queued: Mutex<HashMap<String, Arc<Mutex<Tracked>>>>,
    emit: DispositionEmitFn,
}

impl QueuedInputDispositions {
    pub fn new(emit: DispositionEmitFn) -> Self {
        Self {
            by_message: Mutex::new(HashMap::new()),
            awaiting_queued: Mutex::new(HashMap::new()),
            emit,
        }
    }

    pub fn track(&self, message_key: impl Into<String>, input_id: impl Into<String>) {
        let message_key = message_key.into();
        let input_id = input_id.into();
        let tracked = Arc::new(Mutex::new(Tracked {
            input_id: input_id.clone(),
            phase: Phase::QueuedPending,
        }));
        self.by_message
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(message_key, Arc::clone(&tracked));
        self.awaiting_queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(input_id, tracked);
    }

    pub async fn queued(&self, input_id: Option<&str>) {
        let Some(input_id) = input_id else {
            return;
        };
        (self.emit)(input_id.to_owned(), QueuedDisposition::Queued).await;
        let tracked = self
            .awaiting_queued
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(input_id);
        let Some(tracked) = tracked else {
            return;
        };
        let emit_started = {
            let mut guard = tracked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.phase == Phase::Delivered {
                true
            } else {
                guard.phase = Phase::Queued;
                false
            }
        };
        if emit_started {
            (self.emit)(input_id.to_owned(), QueuedDisposition::Started).await;
        }
    }

    pub async fn delivered(&self, message_key: &str) {
        let tracked = self
            .by_message
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(message_key);
        let Some(tracked) = tracked else {
            return;
        };
        let emit_id = {
            let mut guard = tracked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.phase == Phase::Queued {
                Some(guard.input_id.clone())
            } else {
                guard.phase = Phase::Delivered;
                None
            }
        };
        if let Some(input_id) = emit_id {
            (self.emit)(input_id, QueuedDisposition::Started).await;
        }
    }
}
