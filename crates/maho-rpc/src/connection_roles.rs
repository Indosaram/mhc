//! Connection roles (senpi#3811, `connection-roles.ts`): the capabilities that say who a connection
//! speaks for. A role belongs to one connection, never to a session. Only the client's own
//! `set_client_info` declares one; once declared, a different declaration is refused.
use crate::custom_capability::INTERACTIVE_USER_CAPABILITY;
use std::collections::HashMap;

pub const CONNECTION_ROLE_CAPABILITIES: [&str; 2] = [
    INTERACTIVE_USER_CAPABILITY,
    crate::custom_capability::TASK_PARENT_CAPABILITY,
];
/// The one connection of a single-session `--mode rpc` process.
pub const SOLE_CONNECTION: &str = "sole-connection";
pub const CONNECTION_ROLE_FIXED: &str =
    "connection_role_fixed: interactive_user and task_parent cannot change after they are declared";

pub fn role_connection(connection_id: Option<&str>) -> &str {
    connection_id.unwrap_or(SOLE_CONNECTION)
}

#[derive(Default, Debug)]
pub struct ConnectionRoles {
    declared: HashMap<String, Vec<String>>,
}
impl ConnectionRoles {
    /// Records the roles a `set_client_info` carries; `false` when this connection already declared other roles.
    pub fn declare(&mut self, connection_id: &str, capabilities: &[String]) -> bool {
        let roles: Vec<String> = CONNECTION_ROLE_CAPABILITIES
            .iter()
            .filter(|role| capabilities.iter().any(|capability| capability == *role))
            .map(|role| (*role).to_owned())
            .collect();
        if let Some(prior) = self.declared.get(connection_id) {
            return prior.len() == roles.len() && prior.iter().all(|role| roles.contains(role));
        }
        self.declared.insert(connection_id.to_owned(), roles);
        true
    }
    /// The roles this connection explicitly declared, or `None` while it has declared none.
    pub fn roles_of(&self, connection_id: Option<&str>) -> Option<&[String]> {
        self.declared
            .get(role_connection(connection_id))
            .map(Vec::as_slice)
    }
    /// Takes a declaration made elsewhere (the host, for a worker) unless this connection already declared.
    pub fn adopt(&mut self, connection_id: &str, roles: &[String]) {
        self.declared
            .entry(connection_id.to_owned())
            .or_insert_with(|| roles.to_vec());
    }
    pub fn has(&self, connection_id: Option<&str>, role: &str) -> bool {
        self.declared
            .get(role_connection(connection_id))
            .is_some_and(|roles| roles.iter().any(|value| value == role))
    }
    pub fn release(&mut self, connection_id: &str) {
        self.declared.remove(connection_id);
    }
    pub fn clear(&mut self) {
        self.declared.clear();
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
}
