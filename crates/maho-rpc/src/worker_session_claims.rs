//! Session-path claims a worker session holds (senpi `rpc/worker-session-claims.ts`). Claims precede write
//! grants; native exit also removes claims whose publication is still pending.
//!
//! The Rust reservation set is synchronous, so a claim is decided before [`WorkerSessionClaims::claim`]
//! returns: a path is tracked only while this worker holds it, and a foreign owner is answered at once.
use crate::host_reservations::{BoundPathReservations, SessionPathOwner};
use crate::session_registry::RpcSessionRegistryError;
use std::collections::BTreeMap;

pub struct WorkerSessionClaims {
    reservations: Option<BoundPathReservations>,
    /// Paths this worker holds, with the attachment it last published for each.
    paths: BTreeMap<String, bool>,
    closed: bool,
}

impl WorkerSessionClaims {
    pub fn new(reservations: Option<BoundPathReservations>) -> Self {
        Self {
            reservations,
            paths: BTreeMap::new(),
            closed: false,
        }
    }

    /// `Ok(Some(owner))`: another owner holds the path. `Ok(None)`: this worker holds it (or no
    /// reservations are kept). A closed claim set refuses with `session_closing`.
    pub fn claim(
        &mut self,
        path: &str,
        attached: bool,
    ) -> Result<Option<SessionPathOwner>, RpcSessionRegistryError> {
        if self.closed {
            return Err(RpcSessionRegistryError::new("session_closing", None, None));
        }
        let Some(reservations) = self.reservations.as_mut() else {
            return Ok(None);
        };
        if self.paths.contains_key(path) {
            return Ok(None);
        }
        let owner = reservations.claim(path, attached);
        if owner.is_none() {
            self.paths.insert(path.to_owned(), attached);
        }
        Ok(owner)
    }

    pub fn set_attached(&mut self, attached: bool) {
        let Some(reservations) = self.reservations.as_mut() else {
            return;
        };
        for (path, current) in self.paths.iter_mut() {
            if *current == attached {
                continue;
            }
            *current = attached;
            reservations.set_attached(path, attached);
        }
    }

    pub fn reconcile(&mut self, live_paths: &[String], attached: bool) {
        let stale: Vec<String> = self
            .paths
            .keys()
            .filter(|path| !live_paths.contains(path))
            .cloned()
            .collect();
        for path in stale {
            self.release(&path);
        }
        self.set_attached(attached);
    }

    fn release(&mut self, path: &str) {
        if self.paths.remove(path).is_some()
            && let Some(reservations) = self.reservations.as_mut()
        {
            reservations.release(path);
        }
    }

    pub fn close(&mut self) {
        self.closed = true;
        let held: Vec<String> = self.paths.keys().cloned().collect();
        for path in held {
            self.release(&path);
        }
    }

    pub fn held_paths(&self) -> impl Iterator<Item = &str> {
        self.paths.keys().map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_reservations_every_claim_is_granted_until_closed() {
        let mut claims = WorkerSessionClaims::new(None);
        assert_eq!(claims.claim("/s.jsonl", true), Ok(None));
        claims.close();
        assert_eq!(
            claims.claim("/s.jsonl", true).map_err(|error| error.code),
            Err("session_closing".to_owned())
        );
    }

    #[test]
    fn claims_publish_attachment_and_release_on_reconcile() {
        let dir = tempfile::tempdir().expect("tempdir");
        let reservations = crate::host_reservations::create_session_path_reservations(
            dir.path(),
            "i1".into(),
            None,
            |_| {},
        );
        let mut claims = WorkerSessionClaims::new(Some(reservations));
        let path = dir.path().join("s.jsonl").to_string_lossy().into_owned();
        assert_eq!(claims.claim(&path, true), Ok(None));
        let file =
            crate::host_reservations::reservation_file(&dir.path().join("reservations"), &path);
        assert!(file.exists());
        claims.set_attached(false);
        let published: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&file).expect("read")).expect("json");
        assert_eq!(published["attached"], false);
        claims.reconcile(&[], false);
        assert!(!file.exists());
        assert_eq!(claims.held_paths().count(), 0);
    }
}
