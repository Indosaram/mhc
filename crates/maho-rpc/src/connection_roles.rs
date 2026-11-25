//! Connection roles (`senpi#3811`, `omo-relay-daemon#307`, `connection-roles.ts`): the
//! capabilities that say who a connection speaks for. A role belongs to one connection (or one
//! incarnation of a host-mode delegated socket), never to a session.

use crate::command_authority::{DelegationMode, current_command_authority};
use crate::connection_authority::{effective_roles, read_authority};
use crate::custom_capability::{INTERACTIVE_USER_CAPABILITY, TASK_PARENT_CAPABILITY};
use crate::delegation_record::DelegationDescriptor;
use crate::delegation_resolve::{DelegationResolution, resolve_delegation};
use crate::remote_origin::{RpcRemoteOrigin, same_remote_origin};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

pub const CONNECTION_ROLE_CAPABILITIES: [&str; 2] =
    [INTERACTIVE_USER_CAPABILITY, TASK_PARENT_CAPABILITY];
/// The one connection of a single-session `--mode rpc` process.
pub const SOLE_CONNECTION: &str = "sole-connection";
pub const CONNECTION_ROLE_FIXED: &str =
    "connection_role_fixed: interactive_user and task_parent cannot change after they are declared";
pub const DELEGATION_INCARNATION_STALE: &str = "delegation_incarnation_stale: this declaration was made before detach_connection; declare again";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleDeclaration {
    Declared,
    Fixed,
    Stale,
}

/// What the roles need of a delegated connection: its descriptor and whether its socket is closed.
#[derive(Debug, Clone)]
pub struct DelegatedConnectionView {
    pub descriptor: DelegationDescriptor,
    pub closed: bool,
}

pub type DelegationSource = Arc<dyn Fn(&str) -> Option<DelegatedConnectionView> + Send + Sync>;

/// Untagged claims (local, stdio, legacy-mode delegated connections) live under incarnation 0.
const UNTAGGED: i32 = 0;

pub fn role_connection(connection_id: Option<&str>) -> &str {
    connection_id.unwrap_or(SOLE_CONNECTION)
}

#[derive(Default)]
pub struct ConnectionRoles {
    claims: HashMap<String, HashMap<i32, Vec<String>>>,
    origins: HashMap<String, RpcRemoteOrigin>,
    /// Worker isolate's first-seen delegation per connection (`retain`), kept for the worker's life.
    retained: HashMap<String, DelegationDescriptor>,
    /// Worker isolate's connections whose bind or command carried a delegation.
    carried: HashSet<String>,
    source: Option<DelegationSource>,
}

impl std::fmt::Debug for ConnectionRoles {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnectionRoles")
            .field("claims", &self.claims)
            .field("origins", &self.origins)
            .field("retained", &self.retained)
            .field("carried", &self.carried)
            .finish_non_exhaustive()
    }
}

impl ConnectionRoles {
    /// The host reads each connection's live record from its writer's delegations.
    pub fn use_delegations(&mut self, source: DelegationSource) {
        self.source = Some(source);
    }

    fn view_of(&self, connection_id: &str) -> Option<DelegatedConnectionView> {
        if let Some(source) = &self.source {
            return source(connection_id);
        }
        self.retained
            .get(connection_id)
            .map(|descriptor| DelegatedConnectionView {
                descriptor: descriptor.clone(),
                closed: false,
            })
    }

    /// Whether a declaration carrying `incarnation` must be refused as stale (`omo-relay-daemon#307`).
    pub fn is_stale(&self, connection_id: &str, incarnation: Option<i32>) -> bool {
        let view = self.view_of(connection_id);
        let Some(view) = view
            .as_ref()
            .filter(|v| v.descriptor.mode == DelegationMode::Host)
        else {
            return incarnation.is_some();
        };
        let current = view.descriptor.incarnation;
        if self.source.is_none() {
            return incarnation.is_none_or(|inc| inc < current.unwrap_or(0));
        }
        view.closed || incarnation != current
    }

    /// Backwards-compatible untagged role declaration (`true` = `Declared`, `false` = `Fixed`/`Stale`).
    pub fn declare(&mut self, connection_id: &str, capabilities: &[String]) -> bool {
        self.declare_with_incarnation(connection_id, capabilities, None)
            == RoleDeclaration::Declared
    }

    /// Records the roles a `set_client_info` carries for one incarnation, or why it may not.
    pub fn declare_with_incarnation(
        &mut self,
        connection_id: &str,
        capabilities: &[String],
        incarnation: Option<i32>,
    ) -> RoleDeclaration {
        if self.is_stale(connection_id, incarnation) {
            return RoleDeclaration::Stale;
        }
        let roles: Vec<String> = CONNECTION_ROLE_CAPABILITIES
            .iter()
            .filter(|role| capabilities.iter().any(|capability| capability == *role))
            .map(|role| (*role).to_owned())
            .collect();
        let key = incarnation.unwrap_or(UNTAGGED);
        if let Some(prior) = self.claims.get(connection_id).and_then(|m| m.get(&key)) {
            return if prior.len() == roles.len() && prior.iter().all(|role| roles.contains(role)) {
                RoleDeclaration::Declared
            } else {
                RoleDeclaration::Fixed
            };
        }
        self.claims
            .entry(connection_id.to_owned())
            .or_default()
            .insert(key, roles);
        RoleDeclaration::Declared
    }

    /// The roles declared for `incarnation` (untagged when `None`), or `None` while none were.
    pub fn roles_of(&self, connection_id: Option<&str>) -> Option<&[String]> {
        self.roles_of_incarnation(connection_id, None)
    }

    pub fn roles_of_incarnation(
        &self,
        connection_id: Option<&str>,
        incarnation: Option<i32>,
    ) -> Option<&[String]> {
        self.claims
            .get(role_connection(connection_id))
            .and_then(|m| m.get(&incarnation.unwrap_or(UNTAGGED)))
            .map(Vec::as_slice)
    }

    /// Takes a declaration made elsewhere (the host, for a worker) unless already declared.
    pub fn adopt(&mut self, connection_id: &str, roles: &[String]) {
        self.adopt_with_incarnation(connection_id, roles, None);
    }

    pub fn adopt_with_incarnation(
        &mut self,
        connection_id: &str,
        roles: &[String],
        incarnation: Option<i32>,
    ) {
        let claims = self.claims.entry(connection_id.to_owned()).or_default();
        let key = incarnation.unwrap_or(UNTAGGED);
        claims.retain(|older, _| *older >= key);
        claims.entry(key).or_insert_with(|| roles.to_vec());
    }

    /// Worker isolate: resolves a bind/command's carried delegation against the one retained.
    pub fn retain(
        &mut self,
        connection_id: &str,
        wire: &DelegationDescriptor,
    ) -> DelegationResolution {
        let outcome = resolve_delegation(self.retained.get(connection_id), wire);
        if let Some(retain) = outcome.retain {
            self.retained.insert(connection_id.to_owned(), retain);
        }
        outcome.resolution
    }

    /// Whether `origin` may be declared: none yet or the same again (`senpi#5475`).
    pub fn accepts_origin(&self, connection_id: &str, origin: &RpcRemoteOrigin) -> bool {
        if self.view_of(connection_id).is_some() {
            return true;
        }
        self.origins
            .get(connection_id)
            .is_none_or(|prior| same_remote_origin(prior, origin))
    }

    /// Records the remote device a local connection speaks for; a delegated connection's is ignored.
    pub fn declare_origin(&mut self, connection_id: &str, origin: RpcRemoteOrigin) {
        if self.view_of(connection_id).is_some() {
            return;
        }
        if self.accepts_origin(connection_id, &origin) {
            self.origins.insert(connection_id.to_owned(), origin);
        }
    }

    /// Whether the forwarder stated this delegated connection's device is the host's own account (`senpi#6342`).
    pub fn same_account(&self, connection_id: Option<&str>) -> bool {
        self.view_of(role_connection(connection_id))
            .is_some_and(|view| view.descriptor.principal.same_account == Some(true))
    }

    /// The remote device this connection speaks for: a delegated connection's principal, else declared origin.
    pub fn origin_of(&self, connection_id: Option<&str>) -> Option<RpcRemoteOrigin> {
        let key = role_connection(connection_id);
        if let Some(view) = self.view_of(key) {
            return Some(RpcRemoteOrigin {
                device_id: view.descriptor.principal.device_id,
                device_name: view.descriptor.principal.device_name,
            });
        }
        self.origins.get(key).cloned()
    }

    /// Whether the connection is delegated (a remote device's).
    pub fn is_delegated(&self, connection_id: Option<&str>) -> bool {
        let key = role_connection(connection_id);
        self.view_of(key).is_some() || self.carried.contains(key)
    }

    /// Whether the forwarder itself granted `role` to this connection (`senpi#6305`).
    pub fn granted_by_forwarder(&self, connection_id: Option<&str>, role: &str) -> bool {
        let Some(view) = self.view_of(role_connection(connection_id)) else {
            return false;
        };
        if view.descriptor.mode != DelegationMode::Host {
            return false;
        }
        let Some(authority) = &view.descriptor.authority else {
            return false;
        };
        let state = read_authority(authority);
        !state.retired && state.roles.iter().any(|r| r == role)
    }

    pub fn has(&self, connection_id: Option<&str>, role: &str) -> bool {
        let key = role_connection(connection_id);
        if let Some(ctx) = current_command_authority()
            && role_connection(ctx.connection.as_deref()) == key
        {
            let incarnation = ctx.descriptor.as_ref().and_then(|d| d.incarnation);
            return ctx.has(role, self.roles_of_incarnation(Some(key), incarnation));
        }
        let view = self.view_of(key);
        let descriptor = view.as_ref().map(|v| &v.descriptor);
        let claim = self.roles_of_incarnation(Some(key), descriptor.and_then(|d| d.incarnation));
        let Some(desc) = descriptor else {
            return claim.is_some_and(|roles| roles.iter().any(|r| r == role));
        };
        if desc.mode != DelegationMode::Host {
            return claim.is_some_and(|roles| roles.iter().any(|r| r == role));
        }
        let Some(authority) = &desc.authority else {
            return claim.is_some_and(|roles| roles.iter().any(|r| r == role));
        };
        effective_roles(claim, &read_authority(authority))
            .iter()
            .any(|r| r == role)
    }

    /// A logical detach: drops claims of earlier incarnations of a delegated socket, and everything of a local one.
    pub fn release(&mut self, connection_id: &str) {
        let view = self.view_of(connection_id);
        if view
            .as_ref()
            .is_none_or(|v| v.descriptor.mode != DelegationMode::Host)
        {
            self.claims.remove(connection_id);
            self.origins.remove(connection_id);
            return;
        }
        let current = view
            .and_then(|v| v.descriptor.incarnation)
            .unwrap_or(UNTAGGED);
        if let Some(claims) = self.claims.get_mut(connection_id) {
            claims.retain(|key, _| *key >= current);
        }
    }

    /// The socket is gone: nothing of it is kept.
    pub fn forget(&mut self, connection_id: &str) {
        self.claims.remove(connection_id);
        self.origins.remove(connection_id);
        self.retained.remove(connection_id);
        self.carried.remove(connection_id);
    }

    /// Worker isolate: a bind or command for `connection_id` carried a delegation, in either mode.
    pub fn note_carried_delegation(&mut self, connection_id: &str) {
        self.carried.insert(connection_id.to_owned());
    }

    /// Forgets every connection, for a worker isolate whose session ends.
    pub fn clear(&mut self) {
        self.claims.clear();
        self.origins.clear();
        self.retained.clear();
        self.carried.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_declaration_is_fixed() {
        let mut roles = ConnectionRoles::default();
        assert!(roles.declare("a", &[INTERACTIVE_USER_CAPABILITY.into()]));
        assert!(roles.has(Some("a"), INTERACTIVE_USER_CAPABILITY));
        assert!(roles.declare(
            "a",
            &[INTERACTIVE_USER_CAPABILITY.into(), "question".into()]
        ));
        assert!(!roles.declare("a", &[]));
        assert!(roles.declare("b", &[]));
        assert!(!roles.has(Some("b"), INTERACTIVE_USER_CAPABILITY));
    }

    #[test]
    fn stdio_client_is_the_sole_connection_and_adopt_never_overrides() {
        let mut roles = ConnectionRoles::default();
        roles.adopt(SOLE_CONNECTION, &[INTERACTIVE_USER_CAPABILITY.into()]);
        assert!(roles.has(None, INTERACTIVE_USER_CAPABILITY));
        roles.adopt(SOLE_CONNECTION, &[]);
        assert_eq!(
            roles.roles_of(None),
            Some([INTERACTIVE_USER_CAPABILITY.to_owned()].as_slice())
        );
        roles.release(SOLE_CONNECTION);
        assert!(roles.roles_of(None).is_none());
    }

    #[test]
    fn remote_origin_cannot_change_once_declared_on_local_connection() {
        let mut roles = ConnectionRoles::default();
        let origin = RpcRemoteOrigin {
            device_id: "dev-1".to_owned(),
            device_name: Some("Phone".to_owned()),
        };
        assert!(roles.accepts_origin("c1", &origin));
        roles.declare_origin("c1", origin.clone());
        assert_eq!(roles.origin_of(Some("c1")), Some(origin));
        assert!(!roles.accepts_origin(
            "c1",
            &RpcRemoteOrigin {
                device_id: "dev-2".to_owned(),
                device_name: None,
            }
        ));
    }
}
