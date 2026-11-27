//! One authority snapshot per command (omo-relay-daemon#307). A host-mode delegated line runs inside a
//! `CommandAuthority`: its first role check takes the one atomic load of the captured incarnation's word,
//! every later check in the same command reuses it, and the responses that command produces carry the receipt
//! `authority: {exec, incarnation, epoch, roles}` when the forwarder numbered the record with `relayExec`.

use crate::connection_authority::{
    AuthorityBuffer, AuthorityState, effective_roles, read_authority,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::future::Future;
use std::sync::{Arc, Mutex, PoisonError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DelegationMode {
    Legacy,
    Host,
}

#[derive(Debug, Clone)]
pub struct DelegationDescriptor {
    pub mode: DelegationMode,
    pub incarnation: Option<i32>,
    pub authority: Option<Arc<AuthorityBuffer>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorityReceipt {
    pub exec: u64,
    pub incarnation: Option<i32>,
    pub epoch: Option<u32>,
    /// The effective roles the command's role checks read, or `None` (`null` on wire) when it consulted none.
    pub roles: Option<Vec<String>>,
}

#[derive(Default)]
struct CommandAuthorityCache {
    snapshot: Option<AuthorityState>,
    consulted: Option<Vec<String>>,
}

pub struct CommandAuthority {
    pub connection: Option<String>,
    pub descriptor: Option<DelegationDescriptor>,
    pub exec: Option<u64>,
    /// A worker command of an older incarnation than the one it retained: every role check reads retired.
    stale: bool,
    cache: Mutex<CommandAuthorityCache>,
}

impl CommandAuthority {
    pub fn new(
        connection: Option<String>,
        descriptor: Option<DelegationDescriptor>,
        exec: Option<u64>,
        stale: bool,
    ) -> Self {
        Self {
            connection,
            descriptor,
            exec,
            stale,
            cache: Mutex::new(CommandAuthorityCache::default()),
        }
    }

    pub fn incarnation(&self) -> Option<i32> {
        self.descriptor.as_ref().and_then(|d| d.incarnation)
    }

    fn state(&self) -> Option<AuthorityState> {
        let descriptor = self.descriptor.as_ref()?;
        if descriptor.mode != DelegationMode::Host {
            return None;
        }
        let buffer = descriptor.authority.as_ref()?;
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.snapshot.is_none() {
            cache.snapshot = Some(read_authority(buffer));
        }
        let mut state = cache.snapshot.clone()?;
        if self.stale {
            state.retired = true;
        }
        Some(state)
    }

    /// Whether `claim` (this command's incarnation's claim) grants `role`, read from the one snapshot.
    pub fn has(&self, role: &str, claim: Option<&[String]>) -> bool {
        let Some(state) = self.state() else {
            return claim.is_some_and(|roles| roles.iter().any(|r| r == role));
        };
        let effective = effective_roles(claim, &state);
        let granted = effective.iter().any(|r| r == role);
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.consulted = Some(effective);
        granted
    }

    pub fn receipt(&self) -> Option<AuthorityReceipt> {
        let exec = self.exec?;
        let state = {
            let cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
            cache.snapshot.clone()
        }
        .or_else(|| {
            self.descriptor
                .as_ref()
                .and_then(|d| d.authority.as_ref())
                .map(|buffer| read_authority(buffer))
        });
        let roles = self
            .cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .consulted
            .clone();
        Some(AuthorityReceipt {
            exec,
            incarnation: self.incarnation(),
            epoch: state.map(|s| s.epoch),
            roles,
        })
    }
}

tokio::task_local! {
    static COMMAND_AUTHORITY: Option<Arc<CommandAuthority>>;
}

/// Runs `run` under `authority` in the task-local slot.
pub async fn with_command_authority<F: Future>(
    authority: Arc<CommandAuthority>,
    run: F,
) -> F::Output {
    COMMAND_AUTHORITY.scope(Some(authority), run).await
}

/// Returns the current task-local [`CommandAuthority`], if any.
pub fn current_command_authority() -> Option<Arc<CommandAuthority>> {
    COMMAND_AUTHORITY.try_with(Clone::clone).ok().flatten()
}

/// Runs `run` outside any command's authority, so long-lived background tasks do not inherit it.
pub async fn outside_command<F: Future>(run: F) -> F::Output {
    COMMAND_AUTHORITY.scope(None, run).await
}

/// Adds the running command's receipt to a response addressed to that command's connection. A response that
/// already has a top-level `authority` (a worker's own, stamped where it ran) is never overwritten.
pub fn with_receipt(mut record: Value, target: Option<&str>) -> Value {
    let Some(object) = record.as_object_mut() else {
        return record;
    };
    if object.get("type").and_then(Value::as_str) != Some("response")
        || object.contains_key("authority")
    {
        return record;
    }
    let Some(store) = current_command_authority() else {
        return record;
    };
    if store.connection.as_deref() != target {
        return record;
    }
    if let Some(receipt) = store.receipt()
        && let Ok(value) = serde_json::to_value(receipt)
    {
        object.insert("authority".to_owned(), value);
    }
    record
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayExec {
    Valid(u64),
    Absent,
    Malformed,
}

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

/// Takes the forwarder's `relayExec` out of a host-mode record before anything else sees it: a positive safe
/// integer, absent, or `Malformed` for any other value.
pub fn take_relay_exec(record: &mut Value) -> RelayExec {
    let Some(object) = record.as_object_mut() else {
        return RelayExec::Absent;
    };
    let Some(exec) = object.remove("relayExec") else {
        return RelayExec::Absent;
    };
    match exec.as_u64() {
        Some(n) if (1..=MAX_SAFE_INTEGER).contains(&n) => RelayExec::Valid(n),
        _ => RelayExec::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection_authority::{create_authority, mask_of_roles, store_retired};
    use serde_json::json;

    #[test]
    fn command_authority_caches_first_snapshot_and_handles_stale() {
        let buffer = create_authority(3, mask_of_roles(["interactive_user", "peer_agent"]))
            .expect("authority");
        let descriptor = DelegationDescriptor {
            mode: DelegationMode::Host,
            incarnation: Some(3),
            authority: Some(buffer.clone()),
        };
        let auth =
            CommandAuthority::new(Some("c1".into()), Some(descriptor.clone()), Some(7), false);
        let claim = vec!["interactive_user".to_owned()];
        assert!(auth.has("interactive_user", Some(&claim)));

        // Retiring the buffer afterwards does not affect the snapshot taken on the first check.
        assert!(store_retired(&buffer).expect("retire"));
        assert!(auth.has("interactive_user", Some(&claim)));

        let receipt = auth.receipt().expect("receipt");
        assert_eq!(receipt.exec, 7);
        assert_eq!(receipt.incarnation, Some(3));
        assert_eq!(receipt.epoch, Some(1));
        assert_eq!(receipt.roles, Some(vec!["interactive_user".to_owned()]));

        let stale = CommandAuthority::new(Some("c1".into()), Some(descriptor), Some(8), true);
        assert!(!stale.has("interactive_user", Some(&claim)));
    }

    #[tokio::test]
    async fn with_receipt_stamps_only_matching_connection_response_without_existing_authority() {
        let buffer = create_authority(1, mask_of_roles(["task_parent"])).expect("authority");
        let auth = Arc::new(CommandAuthority::new(
            Some("conn-1".into()),
            Some(DelegationDescriptor {
                mode: DelegationMode::Host,
                incarnation: Some(1),
                authority: Some(buffer),
            }),
            Some(5),
            false,
        ));
        with_command_authority(auth.clone(), async {
            assert!(auth.has("task_parent", Some(&["task_parent".to_owned()])));
            let stamped = with_receipt(json!({"type": "response", "id": "1"}), Some("conn-1"));
            assert_eq!(stamped["authority"]["exec"], 5);
            assert_eq!(stamped["authority"]["roles"], json!(["task_parent"]));

            let wrong_conn = with_receipt(json!({"type": "response", "id": "1"}), Some("conn-2"));
            assert!(wrong_conn.get("authority").is_none());

            let already = with_receipt(
                json!({"type": "response", "authority": {"exec": 99}}),
                Some("conn-1"),
            );
            assert_eq!(already["authority"]["exec"], 99);

            let outside = outside_command(async {
                with_receipt(json!({"type": "response", "id": "1"}), Some("conn-1"))
            })
            .await;
            assert!(outside.get("authority").is_none());
        })
        .await;
    }

    #[test]
    fn take_relay_exec_extracts_and_removes_field() {
        let mut valid = json!({"type": "prompt", "relayExec": 12});
        assert_eq!(take_relay_exec(&mut valid), RelayExec::Valid(12));
        assert!(valid.get("relayExec").is_none());

        let mut absent = json!({"type": "prompt"});
        assert_eq!(take_relay_exec(&mut absent), RelayExec::Absent);

        for bad in [json!(0), json!(-1), json!(1.5), json!("1"), json!(null)] {
            let mut rec = json!({"type": "prompt", "relayExec": bad});
            assert_eq!(take_relay_exec(&mut rec), RelayExec::Malformed);
            assert!(rec.get("relayExec").is_none());
        }
    }
}
