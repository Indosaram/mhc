//! Port of senpi `packages/coding-agent/src/modes/rpc/extension-owner-lifetime.ts`.
//!
//! One session handler's captures, retired synchronously before replacement or disposal awaits.

use crate::extension_owner::{ExtensionOwner, ExtensionTransportOwner};
use std::sync::{Arc, Mutex, PoisonError};

type OwnerSource = Arc<dyn Fn() -> Option<ExtensionOwner> + Send + Sync>;
type RetireCallback = Arc<dyn Fn() + Send + Sync>;

struct Inner {
    generation: String,
    captures: Vec<ExtensionOwner>,
}

pub struct ExtensionOwnerLifetime {
    classic: Option<ExtensionTransportOwner>,
    inner: Mutex<Inner>,
    source: Option<OwnerSource>,
    on_retire_binding: Option<RetireCallback>,
}

impl ExtensionOwnerLifetime {
    pub fn new(
        is_classic: bool,
        source: Option<OwnerSource>,
        on_retire_binding: Option<RetireCallback>,
    ) -> Self {
        let classic = if is_classic && source.is_none() {
            Some(ExtensionTransportOwner::new())
        } else {
            None
        };
        Self {
            classic,
            inner: Mutex::new(Inner {
                generation: uuid::Uuid::new_v4().to_string(),
                captures: Vec::new(),
            }),
            source,
            on_retire_binding,
        }
    }

    pub fn capture(&self) -> Option<ExtensionOwner> {
        let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        let owner = match &self.source {
            None => self
                .classic
                .as_ref()
                .and_then(|c| c.capture(&guard.generation).ok()),
            Some(source) => source(),
        };
        if let Some(ref o) = owner {
            guard.captures.push(o.clone());
        }
        owner
    }

    pub fn retire_binding(&self) {
        let (had_captures, captures) = {
            let mut guard = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let had = !guard.captures.is_empty();
            let drained = std::mem::take(&mut guard.captures);
            guard.generation = uuid::Uuid::new_v4().to_string();
            (had, drained)
        };
        for owner in captures {
            owner.retire();
        }
        if had_captures && let Some(cb) = &self.on_retire_binding {
            cb();
        }
    }

    pub fn dispose(&self) {
        self.retire_binding();
        if let Some(classic) = &self.classic {
            classic.retire();
        }
    }
}
