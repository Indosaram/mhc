//! Port of senpi `packages/coding-agent/src/modes/rpc/extension-owner.ts`.
//!
//! One attachment's shared effect gate plus isolate-local cancellation notification.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionOwnerIdentity {
    pub owner_id: String,
    pub binding_generation: String,
}

#[derive(Clone)]
pub struct ExtensionOwnerWire {
    pub owner_id: String,
    pub binding_generation: String,
    pub retired: Arc<AtomicBool>,
}

/// Commands whose handlers may run extension effects receive a captured owner; control commands never do.
pub fn carries_extension_owner(command_type: &str) -> bool {
    command_type == "extension_request" || command_type.starts_with("eval_cells.")
}

/// One attachment's shared effect gate plus cancellation notification.
#[derive(Clone)]
pub struct ExtensionOwner {
    pub wire: ExtensionOwnerWire,
    cancel_tx: Arc<watch::Sender<bool>>,
    cancel_rx: watch::Receiver<bool>,
}

impl ExtensionOwner {
    pub fn new(wire: ExtensionOwnerWire) -> Self {
        let (cancel_tx, cancel_rx) = watch::channel(wire.retired.load(Ordering::Acquire));
        Self {
            wire,
            cancel_tx: Arc::new(cancel_tx),
            cancel_rx,
        }
    }

    pub fn identity(&self) -> ExtensionOwnerIdentity {
        ExtensionOwnerIdentity {
            owner_id: self.wire.owner_id.clone(),
            binding_generation: self.wire.binding_generation.clone(),
        }
    }

    pub fn retired(&self) -> bool {
        self.wire.retired.load(Ordering::Acquire)
    }

    pub fn cancel_signal(&self) -> watch::Receiver<bool> {
        self.cancel_rx.clone()
    }

    pub fn admit<T>(&self, dispatch: impl FnOnce() -> T) -> Result<T, &'static str> {
        if self.retired() {
            return Err("RPC owner retired");
        }
        Ok(dispatch())
    }

    pub fn retire(&self) {
        self.wire.retired.store(true, Ordering::Release);
        let _ = self.cancel_tx.send(true);
    }
}

/// Minted once per actual transport registration, independently of its reusable routing ID.
pub struct ExtensionTransportOwner {
    pub owner_id: String,
    bindings: Mutex<HashMap<String, ExtensionOwner>>,
    closed: AtomicBool,
}

impl Default for ExtensionTransportOwner {
    fn default() -> Self {
        Self::new()
    }
}

impl ExtensionTransportOwner {
    pub fn new() -> Self {
        Self {
            owner_id: uuid::Uuid::new_v4().to_string(),
            bindings: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        }
    }

    pub fn retired(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub fn capture(&self, binding_generation: &str) -> Result<ExtensionOwner, &'static str> {
        if self.retired() {
            return Err("RPC transport retired");
        }
        let mut guard = self.bindings.lock().unwrap_or_else(PoisonError::into_inner);
        if self.retired() {
            return Err("RPC transport retired");
        }
        if let Some(existing) = guard.get(binding_generation) {
            return Ok(existing.clone());
        }
        let owner = ExtensionOwner::new(ExtensionOwnerWire {
            owner_id: self.owner_id.clone(),
            binding_generation: binding_generation.to_owned(),
            retired: Arc::new(AtomicBool::new(false)),
        });
        guard.insert(binding_generation.to_owned(), owner.clone());
        Ok(owner)
    }

    /// The gate already issued for a generation, without minting one.
    pub fn peek(&self, binding_generation: &str) -> Option<ExtensionOwner> {
        if self.retired() {
            return None;
        }
        self.bindings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(binding_generation)
            .cloned()
    }

    pub fn retire_binding(&self, binding_generation: &str) {
        if let Some(owner) = self
            .bindings
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(binding_generation)
            .cloned()
        {
            owner.retire();
        }
    }

    pub fn retire(&self) {
        self.closed.store(true, Ordering::Release);
        let owners: Vec<_> = {
            let mut guard = self.bindings.lock().unwrap_or_else(PoisonError::into_inner);
            guard.drain().map(|(_, v)| v).collect()
        };
        for owner in owners {
            owner.retire();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retire_prevents_admission_and_signals_cancel() {
        let transport = ExtensionTransportOwner::new();
        let owner = transport.capture("gen-1").expect("capture");
        let sig = owner.cancel_signal();
        assert!(!*sig.borrow());
        assert_eq!(owner.admit(|| 42), Ok(42));
        transport.retire_binding("gen-1");
        assert!(owner.retired());
        assert!(*sig.borrow());
        assert_eq!(owner.admit(|| 42), Err("RPC owner retired"));
        transport.retire();
        assert!(transport.capture("gen-2").is_err());
    }
}
