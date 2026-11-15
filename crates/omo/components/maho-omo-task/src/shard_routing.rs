//! Port of omo `packages/omo-senpi/src/components/task/shard-routing.ts`.

use serde_json::Value;

use maho_rpc::host_endpoints::{ShardKind, shard_key};
use senpi_task::runners::rpc_host::own_endpoint::read_own_host_socket;
use senpi_task::runners::rpc_host::session_role::read_session_context;

pub const INHERITED_SHARD_OWNER: &str = "<inherited>";
pub const SHARD_KEY_CONTEXT: &str = "shard_key";
const SHARD_KEY_SHAPE_LEN: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardIdentity {
    pub kind: ShardKind,
    pub key: String,
    pub owner_session_id: String,
    pub owner_session_file: Option<String>,
    pub inherited: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardRoutingError {
    ShardIdentityMissing,
}

impl std::fmt::Display for ShardRoutingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ShardIdentityMissing => write!(
                formatter,
                "shard_identity_missing: a process child was routed before session_start attached the session"
            ),
        }
    }
}

impl std::error::Error for ShardRoutingError {}

pub trait SessionIdentitySource {
    fn session_id(&self) -> Option<String>;
    fn session_file(&self) -> Option<String>;
}

pub fn inherited_shard_key(pi: &Value) -> Option<String> {
    let key = read_session_context(pi)?;
    let key = key.get(SHARD_KEY_CONTEXT)?;
    (key.len() == SHARD_KEY_SHAPE_LEN
        && key
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then(|| key.clone())
}

pub fn session_shard_identity(
    runtime: &dyn SessionIdentitySource,
    pi: &Value,
) -> Result<ShardIdentity, ShardRoutingError> {
    if let Some(inherited_key) = inherited_shard_key(pi) {
        return Ok(ShardIdentity {
            kind: ShardKind::P,
            key: inherited_key,
            owner_session_id: INHERITED_SHARD_OWNER.to_owned(),
            owner_session_file: None,
            inherited: true,
        });
    }
    let session_id = runtime
        .session_id()
        .filter(|session_id| !session_id.is_empty())
        .ok_or(ShardRoutingError::ShardIdentityMissing)?;
    Ok(ShardIdentity {
        kind: ShardKind::P,
        key: shard_key(ShardKind::P, &session_id),
        owner_session_id: session_id,
        owner_session_file: runtime.session_file(),
        inherited: false,
    })
}

pub fn own_host_socket(pi: &Value) -> Option<String> {
    read_own_host_socket(pi)
}

pub fn inside_host(pi: &Value) -> bool {
    inherited_shard_key(pi).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Option<&'static str>, Option<&'static str>);

    impl SessionIdentitySource for Fixed {
        fn session_id(&self) -> Option<String> {
            self.0.map(str::to_owned)
        }
        fn session_file(&self) -> Option<String> {
            self.1.map(str::to_owned)
        }
    }

    #[test]
    fn a_root_session_keys_its_own_tree() {
        let identity =
            session_shard_identity(&Fixed(Some("sess-1"), Some("/s.json")), &Value::Null)
                .expect("root");
        assert!(!identity.inherited);
        assert_eq!(identity.owner_session_id, "sess-1");
        assert_eq!(identity.key, shard_key(ShardKind::P, "sess-1"));
        assert_eq!(identity.owner_session_file.as_deref(), Some("/s.json"));
    }

    #[test]
    fn a_child_reuses_the_tree_key_it_inherits() {
        let key = shard_key(ShardKind::P, "root-session");
        let pi = serde_json::json!({"sessionContext": {"shard_key": key}});
        let identity =
            session_shard_identity(&Fixed(Some("child-2"), None), &pi).expect("inherited");
        assert!(identity.inherited);
        assert_eq!(identity.owner_session_id, INHERITED_SHARD_OWNER);
        assert_eq!(identity.key, key);
        assert_eq!(identity.owner_session_file, None);
        assert!(inside_host(&pi));
    }

    #[test]
    fn a_malformed_inherited_key_is_ignored() {
        let pi = serde_json::json!({"sessionContext": {"shard_key": "NOT-HEX"}});
        assert_eq!(inherited_shard_key(&pi), None);
        assert!(!inside_host(&pi));
        let identity = session_shard_identity(&Fixed(Some("s"), None), &pi).expect("root");
        assert!(!identity.inherited);
    }

    #[test]
    fn routing_before_session_start_is_refused() {
        assert_eq!(
            session_shard_identity(&Fixed(None, None), &Value::Null),
            Err(ShardRoutingError::ShardIdentityMissing)
        );
        assert_eq!(
            session_shard_identity(&Fixed(Some(""), None), &Value::Null),
            Err(ShardRoutingError::ShardIdentityMissing)
        );
    }

    #[test]
    fn the_own_socket_is_read_from_the_session_context() {
        assert_eq!(own_host_socket(&Value::Null), None);
        let pi = serde_json::json!({"sessionContext": {"host_socket": "/rpc/shards/p-1.sock"}});
        assert_eq!(
            own_host_socket(&pi).as_deref(),
            Some("/rpc/shards/p-1.sock")
        );
    }
}
