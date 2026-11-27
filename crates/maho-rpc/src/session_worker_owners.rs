//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-owners.ts`.
//!
//! Host half (`WorkerOwnerGates`) and isolate half (`IssuedWorkerOwners`) of a worker binding's
//! extension owners (#4728). Gates are captured at ingress, before the command's first await, and
//! retired atomically before the isolate hears about it.

use maho_ai::utils::abort::{AbortController, AbortSignal};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExtensionOwnerIdentity {
    pub owner_id: String,
    pub binding_generation: String,
}

#[derive(Clone)]
pub struct ExtensionOwnerWire {
    pub owner_id: String,
    pub binding_generation: String,
    pub retirement: Arc<AtomicI32>,
}

/// Commands whose handlers may run extension effects receive a captured owner; control commands
/// never do.
pub fn carries_extension_owner(command: &Value) -> bool {
    let kind = command.get("type").and_then(Value::as_str).unwrap_or("");
    kind == "extension_request" || kind.starts_with("eval_cells.")
}

#[derive(Clone)]
pub struct ExtensionOwner {
    pub wire: ExtensionOwnerWire,
    cancellation: AbortController,
}

impl ExtensionOwner {
    pub fn new(wire: ExtensionOwnerWire) -> Self {
        Self {
            wire,
            cancellation: AbortController::new(),
        }
    }

    pub fn retired(&self) -> bool {
        self.wire.retirement.load(Ordering::Acquire) != 0
    }

    pub fn disconnect_signal(&self) -> AbortSignal {
        self.cancellation.signal()
    }

    pub fn admit<T, F: FnOnce() -> T>(&self, dispatch: F) -> Result<T, &'static str> {
        if self.retired() {
            return Err("RPC owner retired");
        }
        Ok(dispatch())
    }

    pub fn retire(&self) {
        self.wire.retirement.store(1, Ordering::Release);
        self.cancellation.abort(None);
    }
}

/// Minted once per actual transport registration, independently of its reusable routing ID.
pub struct ExtensionTransportOwner {
    pub owner_id: String,
    bindings: HashMap<String, ExtensionOwner>,
    closed: bool,
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
            bindings: HashMap::new(),
            closed: false,
        }
    }

    pub fn retired(&self) -> bool {
        self.closed
    }

    pub fn capture(&mut self, binding_generation: &str) -> Result<ExtensionOwner, &'static str> {
        if self.closed {
            return Err("RPC transport retired");
        }
        if let Some(existing) = self.bindings.get(binding_generation) {
            return Ok(existing.clone());
        }
        let owner = ExtensionOwner::new(ExtensionOwnerWire {
            owner_id: self.owner_id.clone(),
            binding_generation: binding_generation.to_owned(),
            retirement: Arc::new(AtomicI32::new(0)),
        });
        self.bindings
            .insert(binding_generation.to_owned(), owner.clone());
        Ok(owner)
    }

    pub fn peek(&self, binding_generation: &str) -> Option<&ExtensionOwner> {
        if self.closed {
            None
        } else {
            self.bindings.get(binding_generation)
        }
    }

    pub fn retire_binding(&self, binding_generation: &str) {
        if let Some(owner) = self.bindings.get(binding_generation) {
            owner.retire();
        }
    }

    pub fn retire(&mut self) {
        self.closed = true;
        for owner in self.bindings.values() {
            owner.retire();
        }
        self.bindings.clear();
    }
}

fn identity_key(identity: &ExtensionOwnerIdentity) -> (String, String) {
    (
        identity.owner_id.clone(),
        identity.binding_generation.clone(),
    )
}

/// Host half of a worker binding's extension owners (#4728).
pub struct WorkerOwnerGates {
    generation: String,
    issued: HashMap<String, Vec<ExtensionOwner>>,
}

impl Default for WorkerOwnerGates {
    fn default() -> Self {
        Self {
            generation: uuid::Uuid::new_v4().to_string(),
            issued: HashMap::new(),
        }
    }
}

impl WorkerOwnerGates {
    pub fn capture(
        &mut self,
        transport: Option<&mut ExtensionTransportOwner>,
        connection: Option<&str>,
        command: &Value,
    ) -> Option<ExtensionOwnerWire> {
        let connection = connection?;
        let transport = transport?;
        if !carries_extension_owner(command) {
            return None;
        }
        let mut owner = transport.capture(&self.generation).ok()?;
        if owner.retired() {
            self.generation = uuid::Uuid::new_v4().to_string();
            owner = transport.capture(&self.generation).ok()?;
        }
        let owners = self.issued.entry(connection.to_owned()).or_default();
        if !owners.iter().any(|existing| {
            existing.wire.owner_id == owner.wire.owner_id
                && existing.wire.binding_generation == owner.wire.binding_generation
        }) {
            owners.push(owner.clone());
        }
        Some(owner.wire)
    }

    /// `connection` left this session. Only the incarnation that left is retired.
    pub fn detach(
        &mut self,
        current_owner_id: Option<&str>,
        connection: &str,
    ) -> Vec<ExtensionOwnerIdentity> {
        let Some(owners) = self.issued.remove(connection) else {
            return Vec::new();
        };
        let mut kept = Vec::new();
        let mut retired = Vec::new();
        for owner in owners {
            if let Some(current_id) = current_owner_id
                && current_id != owner.wire.owner_id
                && !owner.retired()
            {
                kept.push(owner);
            } else {
                owner.retire();
                retired.push(ExtensionOwnerIdentity {
                    owner_id: owner.wire.owner_id,
                    binding_generation: owner.wire.binding_generation,
                });
            }
        }
        if !kept.is_empty() {
            self.issued.insert(connection.to_owned(), kept);
        }
        retired
    }

    pub fn retire_all(&mut self) -> Vec<ExtensionOwnerIdentity> {
        let mut seen = HashSet::new();
        let mut retired = Vec::new();
        for owners in self.issued.values() {
            for owner in owners {
                owner.retire();
                let id = ExtensionOwnerIdentity {
                    owner_id: owner.wire.owner_id.clone(),
                    binding_generation: owner.wire.binding_generation.clone(),
                };
                if seen.insert(id.clone()) {
                    retired.push(id);
                }
            }
        }
        self.issued.clear();
        self.generation = uuid::Uuid::new_v4().to_string();
        retired
    }
}

/// Isolate half: one owner object per issued gate, sharing the host's retirement word.
#[derive(Default)]
pub struct IssuedWorkerOwners {
    owners: HashMap<(String, String), ExtensionOwner>,
}

impl IssuedWorkerOwners {
    pub fn adopt(&mut self, wire: ExtensionOwnerWire) -> ExtensionOwner {
        let id = ExtensionOwnerIdentity {
            owner_id: wire.owner_id.clone(),
            binding_generation: wire.binding_generation.clone(),
        };
        let key = identity_key(&id);
        if let Some(existing) = self.owners.get(&key) {
            return existing.clone();
        }
        let owner = ExtensionOwner::new(wire);
        self.owners.insert(key, owner.clone());
        owner
    }

    pub fn retire(&mut self, identities: &[ExtensionOwnerIdentity]) {
        for identity in identities {
            let key = identity_key(identity);
            if let Some(owner) = self.owners.remove(&key) {
                owner.retire();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn owner_gates_capture_detach_and_retire_exact_incarnation() {
        let mut gates = WorkerOwnerGates::default();
        let mut transport_a = ExtensionTransportOwner::new();
        let transport_b = ExtensionTransportOwner::new();
        assert!(
            gates
                .capture(
                    Some(&mut transport_a),
                    Some("c1"),
                    &json!({"type":"prompt"})
                )
                .is_none()
        );

        let wire_a = gates
            .capture(
                Some(&mut transport_a),
                Some("c1"),
                &json!({"type":"extension_request"}),
            )
            .expect("captured");
        let mut issued = IssuedWorkerOwners::default();
        let adopted = issued.adopt(wire_a.clone());
        assert!(!adopted.retired());
        assert_eq!(adopted.admit(|| 7), Ok(7));

        // If connection "c1" now belongs to transport_b, detaching leaves transport_a's gate?
        // Wait: detaching with current_owner_id = transport_b.owner_id keeps transport_a's non-retired gate
        // under c1 if it belonged to a different transport, whereas detaching when current_owner_id matches
        // or is None retires it.
        let kept_retired = gates.detach(Some(&transport_b.owner_id), "c1");
        assert!(kept_retired.is_empty());
        assert!(!adopted.retired());

        let retired = gates.detach(Some(&transport_a.owner_id), "c1");
        assert_eq!(retired.len(), 1);
        assert!(adopted.retired());
        assert!(adopted.admit(|| 7).is_err());

        issued.retire(&retired);
    }
}
