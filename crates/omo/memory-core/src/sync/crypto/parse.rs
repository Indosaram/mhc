use super::envelope::{exact, integer, text};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Principal {
    pub kind: String,
    pub id: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Survivable {
    Synced,
    Roaming,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryKey {
    pub credential_id: String,
    pub rp_id: String,
    pub enrollment_nonce: String,
    pub sealing_public_key: String,
    pub signing_public_key: String,
    pub survivable: Survivable,
}
#[derive(Debug, Clone)]
pub enum ParsedBody {
    Genesis {
        first_device: Principal,
        recovery_key: RecoveryKey,
        bootstrap: Value,
        custody_root: String,
        countersign: String,
        wraps_digest: String,
    },
    RecoveryKey {
        key: RecoveryKey,
        countersign: String,
        wraps_digest: String,
    },
    RecoveryKeyRemoved {
        credential_id: String,
    },
    Approval {
        principal: Principal,
        wraps_digest: String,
        commitment: String,
        joining_nonce: String,
    },
    Rotation {
        members: Vec<Principal>,
        wraps_digest: String,
        fork_resolution: Option<u64>,
    },
    RecoveryJoin {
        keyring_head: String,
        principal: Principal,
        credential_id: String,
    },
    RecoveryJoinVetoed {
        join_digest: String,
    },
    ForkResolution {
        members: Vec<Principal>,
        recovery_keys: Vec<RecoveryKey>,
        epoch: u64,
    },
}
impl ParsedBody {
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Genesis { .. } => "genesis",
            Self::RecoveryKey { .. } => "recovery-key",
            Self::RecoveryKeyRemoved { .. } => "recovery-key-removed",
            Self::Approval { .. } => "approval",
            Self::Rotation { .. } => "rotation",
            Self::RecoveryJoin { .. } => "recovery-join",
            Self::RecoveryJoinVetoed { .. } => "recovery-join-vetoed",
            Self::ForkResolution { .. } => "fork-resolution",
        }
    }
}
pub fn parse_principal(v: &Value) -> Option<Principal> {
    if !exact(
        v,
        &["kind", "id", "sealing_public_key", "signing_public_key"],
    ) || v["kind"] != "device"
    {
        return None;
    }
    Some(Principal {
        kind: "device".into(),
        id: text(&v["id"])?.into(),
        sealing_public_key: text(&v["sealing_public_key"])?.into(),
        signing_public_key: text(&v["signing_public_key"])?.into(),
    })
}
const RECOVERY_KEYS: &[&str] = &[
    "credential_id",
    "rp_id",
    "enrollment_nonce",
    "sealing_public_key",
    "signing_public_key",
    "survivable",
];
fn read_recovery(v: &Value) -> Option<RecoveryKey> {
    Some(RecoveryKey {
        credential_id: text(&v["credential_id"])?.into(),
        rp_id: text(&v["rp_id"])?.into(),
        enrollment_nonce: text(&v["enrollment_nonce"])?.into(),
        sealing_public_key: text(&v["sealing_public_key"])?.into(),
        signing_public_key: text(&v["signing_public_key"])?.into(),
        survivable: match v["survivable"].as_str()? {
            "synced" => Survivable::Synced,
            "roaming" => Survivable::Roaming,
            _ => return None,
        },
    })
}
pub fn parse_recovery(v: &Value) -> Option<RecoveryKey> {
    if !exact(v, RECOVERY_KEYS) {
        return None;
    }
    read_recovery(v)
}
fn shaped(body: &Value, fields: &[&str]) -> bool {
    let mut keys = vec!["kind", "account", "epoch"];
    keys.extend(fields);
    exact(body, &keys)
}
pub fn parse_body(b: &Value) -> Option<ParsedBody> {
    b["account"].as_str()?;
    integer(&b["epoch"], 1)?;
    match b["kind"].as_str()? {
        "genesis" => {
            if !shaped(
                b,
                &["bootstrap", "custody_root", "countersign", "wraps_digest"],
            ) {
                return None;
            }
            let boot = &b["bootstrap"];
            if !exact(
                boot,
                &["account", "custody_nonce", "first_device", "recovery_key"],
            ) || boot["account"] != b["account"]
            {
                return None;
            }
            text(&boot["custody_nonce"])?;
            Some(ParsedBody::Genesis {
                first_device: parse_principal(&boot["first_device"])?,
                recovery_key: parse_recovery(&boot["recovery_key"])?,
                bootstrap: boot.clone(),
                custody_root: text(&b["custody_root"])?.into(),
                countersign: text(&b["countersign"])?.into(),
                wraps_digest: text(&b["wraps_digest"])?.into(),
            })
        }
        "recovery-key" => {
            let mut keys = RECOVERY_KEYS.to_vec();
            keys.extend(["countersign", "wraps_digest"]);
            if !shaped(b, &keys) {
                return None;
            }
            Some(ParsedBody::RecoveryKey {
                key: read_recovery(b)?,
                countersign: text(&b["countersign"])?.into(),
                wraps_digest: text(&b["wraps_digest"])?.into(),
            })
        }
        "recovery-key-removed" => {
            if !shaped(b, &["credential_id"]) {
                return None;
            }
            Some(ParsedBody::RecoveryKeyRemoved {
                credential_id: text(&b["credential_id"])?.into(),
            })
        }
        "approval" => {
            let pairing = &b["pairing"];
            if !shaped(b, &["principal", "wraps_digest", "pairing"])
                || !exact(pairing, &["commitment", "joining_nonce", "approver_nonce"])
            {
                return None;
            }
            text(&pairing["approver_nonce"])?;
            Some(ParsedBody::Approval {
                principal: parse_principal(&b["principal"])?,
                wraps_digest: text(&b["wraps_digest"])?.into(),
                commitment: text(&pairing["commitment"])?.into(),
                joining_nonce: text(&pairing["joining_nonce"])?.into(),
            })
        }
        "rotation" => {
            if !shaped(b, &["members", "wraps_digest", "fork_resolution"]) {
                return None;
            }
            let members = b["members"]
                .as_array()?
                .iter()
                .map(parse_principal)
                .collect::<Option<Vec<_>>>()?;
            if members.is_empty() {
                return None;
            }
            Some(ParsedBody::Rotation {
                members,
                wraps_digest: text(&b["wraps_digest"])?.into(),
                fork_resolution: if b["fork_resolution"].is_null() {
                    None
                } else {
                    Some(integer(&b["fork_resolution"], 1)?)
                },
            })
        }
        "recovery-join" => {
            if !shaped(b, &["keyring_head", "principal", "credential_id"]) {
                return None;
            }
            Some(ParsedBody::RecoveryJoin {
                keyring_head: text(&b["keyring_head"])?.into(),
                principal: parse_principal(&b["principal"])?,
                credential_id: text(&b["credential_id"])?.into(),
            })
        }
        "recovery-join-vetoed" => {
            if !shaped(b, &["join_digest"]) {
                return None;
            }
            Some(ParsedBody::RecoveryJoinVetoed {
                join_digest: text(&b["join_digest"])?.into(),
            })
        }
        "fork-resolution" => {
            let state = &b["state"];
            if !shaped(
                b,
                &[
                    "fork_set",
                    "evidence",
                    "continuation",
                    "state",
                    "merged_digest",
                ],
            ) || !exact(state, &["members", "recovery_keys", "epoch"])
            {
                return None;
            }
            Some(ParsedBody::ForkResolution {
                members: state["members"]
                    .as_array()?
                    .iter()
                    .map(parse_principal)
                    .collect::<Option<_>>()?,
                recovery_keys: state["recovery_keys"]
                    .as_array()?
                    .iter()
                    .map(parse_recovery)
                    .collect::<Option<_>>()?,
                epoch: integer(&state["epoch"], 1)?,
            })
        }
        _ => None,
    }
}
