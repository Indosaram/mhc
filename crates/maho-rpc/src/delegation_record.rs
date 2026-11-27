//! Port of senpi `packages/coding-agent/src/modes/rpc/delegation-record.ts`.
//!
//! A delegated socket's state for its whole life (`omo-relay-daemon#307`): created at its first
//! `declare_delegation`, kept through `detach_connection` and physical close, deleted only once
//! the socket is gone (`forget`).

use std::collections::HashSet;
use std::sync::Arc;

pub use crate::command_authority::DelegationMode;
use crate::connection_authority::{
    AuthorityBuffer, INCARNATION_MAX, create_authority, mask_of_roles, next_incarnation,
    read_authority, roles_of_mask, store_narrowed, store_retired,
};
use crate::delegation_declaration::{Declaration, Principal, same_principal};
use crate::method_classes::RpcMethodClass;

/// What a line captures before its first await and what `bind`/`command` carry to a worker.
#[derive(Debug, Clone)]
pub struct DelegationDescriptor {
    pub principal: Principal,
    pub mode: DelegationMode,
    /// Host mode: this incarnation's number, also stamped into `authority[1]`.
    pub incarnation: Option<i32>,
    /// Host mode: this incarnation's word at index 0.
    pub authority: Option<Arc<AuthorityBuffer>>,
}

impl DelegationDescriptor {
    pub fn to_command_descriptor(&self) -> crate::command_authority::DelegationDescriptor {
        crate::command_authority::DelegationDescriptor {
            mode: self.mode,
            incarnation: self.incarnation,
            authority: self.authority.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct DelegationIncarnation {
    pub incarnation: i32,
    pub authority: Arc<AuthorityBuffer>,
}

#[derive(Debug, Clone)]
pub struct DelegationRecord {
    pub principal: Principal,
    pub mode: DelegationMode,
    pub granted: HashSet<RpcMethodClass>,
    pub current: Option<DelegationIncarnation>,
    pub last_incarnation: i32,
    pub closed: bool,
}

pub fn create_record(
    principal: Principal,
    granted: HashSet<RpcMethodClass>,
    roles: Option<&HashSet<String>>,
) -> DelegationRecord {
    let Some(roles) = roles else {
        return DelegationRecord {
            principal,
            mode: DelegationMode::Legacy,
            granted,
            current: None,
            last_incarnation: 0,
            closed: false,
        };
    };
    let mask = mask_of_roles(roles.iter().map(String::as_str));
    let authority = create_authority(1, mask)
        .unwrap_or_else(|_| unreachable!("incarnation 1 with masked roles is always valid"));
    DelegationRecord {
        principal,
        mode: DelegationMode::Host,
        granted,
        current: Some(DelegationIncarnation {
            incarnation: 1,
            authority,
        }),
        last_incarnation: 1,
        closed: false,
    }
}

/// A later declaration on an existing record: the principal stays, `granted` and `roles` only
/// narrow (`delegation_fixed` otherwise; `roles` on a legacy record too). Omitted `roles` keeps
/// the mask. Returns `false` when it must be refused.
pub fn narrow_record(record: &mut DelegationRecord, parsed: &Declaration) -> bool {
    if !same_principal(&record.principal, &parsed.principal) {
        return false;
    }
    if parsed
        .granted
        .iter()
        .any(|class| !record.granted.contains(class))
    {
        return false;
    }
    if let Some(ref roles) = parsed.roles {
        let Some(ref current) = record.current else {
            return false;
        };
        let mask_roles = read_authority(&current.authority).roles;
        if roles.iter().any(|role| !mask_roles.contains(role)) {
            return false;
        }
        let _ = store_narrowed(&current.authority, roles.iter().map(String::as_str));
    }
    record.granted = parsed.granted.clone();
    true
}

/// The `data.roles` echo of a host-mode declaration reply: the mask after it, in canonical order.
pub fn echoed_roles(record: &DelegationRecord) -> Option<Vec<String>> {
    record
        .current
        .as_ref()
        .map(|current| roles_of_mask(read_authority(&current.authority).mask))
}

pub fn descriptor_of(record: &DelegationRecord) -> DelegationDescriptor {
    match &record.current {
        None => DelegationDescriptor {
            principal: record.principal.clone(),
            mode: record.mode,
            incarnation: None,
            authority: None,
        },
        Some(current) => DelegationDescriptor {
            principal: record.principal.clone(),
            mode: record.mode,
            incarnation: Some(current.incarnation),
            authority: Some(Arc::clone(&current.authority)),
        },
    }
}

/// `detach_connection` on a host-mode record: retire `k`, then issue `k+1` with `k`'s mask at
/// epoch 1. Returns `false` when the counter is exhausted: `k` stays retired, the record is
/// closed, and the caller ends the socket.
pub fn rotate_incarnation(record: &mut DelegationRecord, max: Option<i32>) -> bool {
    let max = max.unwrap_or(INCARNATION_MAX);
    let Some(ref current) = record.current else {
        return true;
    };
    if record.mode != DelegationMode::Host || record.closed {
        return true;
    }
    let _ = store_retired(&current.authority);
    let Some(next) = next_incarnation(record.last_incarnation, Some(max)) else {
        record.closed = true;
        return false;
    };
    let mask = read_authority(&current.authority).mask;
    let Ok(authority) = create_authority(next, mask) else {
        record.closed = true;
        return false;
    };
    record.last_incarnation = next;
    record.current = Some(DelegationIncarnation {
        incarnation: next,
        authority,
    });
    true
}

/// Physical close: the current word reads retired from now on; the record stays until the socket
/// is forgotten.
pub fn close_record(record: &mut DelegationRecord) {
    if let Some(ref current) = record.current {
        let _ = store_retired(&current.authority);
    }
    record.closed = true;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal() -> Principal {
        Principal {
            device_id: "dev-1".to_owned(),
            device_name: None,
            conn: None,
            same_account: None,
        }
    }

    #[test]
    fn host_record_narrows_rotates_and_retires_on_exhaustion() {
        let roles = HashSet::from(["interactive_user".to_owned(), "peer_agent".to_owned()]);
        let granted = HashSet::from([RpcMethodClass::Observe, RpcMethodClass::Drive]);
        let mut rec = create_record(principal(), granted, Some(&roles));
        assert_eq!(
            echoed_roles(&rec),
            Some(vec!["interactive_user".to_owned(), "peer_agent".to_owned()])
        );

        let narrow_ok = Declaration {
            principal: principal(),
            granted: HashSet::from([RpcMethodClass::Observe]),
            roles: Some(HashSet::from(["peer_agent".to_owned()])),
        };
        assert!(narrow_record(&mut rec, &narrow_ok));
        assert_eq!(echoed_roles(&rec), Some(vec!["peer_agent".to_owned()]));

        // Cannot widen back.
        let widen = Declaration {
            principal: principal(),
            granted: HashSet::from([RpcMethodClass::Observe, RpcMethodClass::Drive]),
            roles: None,
        };
        assert!(!narrow_record(&mut rec, &widen));

        let first_auth = rec.current.as_ref().expect("k=1").authority.clone();
        assert!(rotate_incarnation(&mut rec, Some(2)));
        assert!(read_authority(&first_auth).retired);
        assert_eq!(rec.last_incarnation, 2);
        assert_eq!(echoed_roles(&rec), Some(vec!["peer_agent".to_owned()]));

        // Exhausted at max=2.
        assert!(!rotate_incarnation(&mut rec, Some(2)));
        assert!(rec.closed);
    }

    #[test]
    fn legacy_record_refuses_roles_and_closes_cleanly() {
        let mut rec = create_record(principal(), HashSet::from([RpcMethodClass::Observe]), None);
        assert_eq!(echoed_roles(&rec), None);
        let with_roles = Declaration {
            principal: principal(),
            granted: HashSet::from([RpcMethodClass::Observe]),
            roles: Some(HashSet::new()),
        };
        assert!(!narrow_record(&mut rec, &with_roles));
        close_record(&mut rec);
        assert!(rec.closed);
    }
}
