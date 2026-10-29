use super::{
    RecoveryError,
    ports::{RecoverySaltInput, RecoveryTransport},
};
use std::collections::{BTreeMap, BTreeSet};
pub const MAX_RECOVERY_KEYS: usize = 16;
#[derive(Debug, PartialEq, Eq)]
pub enum SaltRefusal {
    Unavailable,
    TooManyRecoveryKeys,
}
pub fn parse_salt_inputs(keys: &serde_json::Value) -> Result<Vec<RecoverySaltInput>, SaltRefusal> {
    let keys = keys.as_array().ok_or(SaltRefusal::Unavailable)?;
    let mut found = BTreeMap::new();
    let mut order = Vec::new();
    let mut removed = BTreeSet::new();
    for row in keys {
        let body = &row["body"];
        if body["kind"] == "recovery-key-removed"
            && let Some(id) = body["credential_id"].as_str()
        {
            removed.insert(id);
        }
        let salt = match body["kind"].as_str() {
            Some("genesis") => &body["bootstrap"]["recovery_key"],
            Some("recovery-key") => body,
            _ => continue,
        };
        if let (Some(credential), Some(nonce)) = (
            salt["credential_id"].as_str(),
            salt["enrollment_nonce"].as_str(),
        ) && !found.contains_key(credential)
        {
            order.push(credential);
            found.insert(credential, nonce);
        }
    }
    order.retain(|id| !removed.contains(id));
    if order.len() > MAX_RECOVERY_KEYS {
        return Err(SaltRefusal::TooManyRecoveryKeys);
    }
    Ok(order
        .into_iter()
        .map(|id| RecoverySaltInput {
            credential_id: id.into(),
            enrollment_nonce: found[id].into(),
        })
        .collect())
}
pub async fn served_salt_inputs(
    transport: &dyn RecoveryTransport,
) -> Result<Result<Vec<RecoverySaltInput>, SaltRefusal>, RecoveryError> {
    Ok(parse_salt_inputs(&transport.list_keys().await?))
}
