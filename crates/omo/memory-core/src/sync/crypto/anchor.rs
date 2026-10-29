use super::super::keyring_port::DeviceKeyring;
use super::{
    entry::KeyringEntry,
    fold::Authority,
    parse::{ParsedBody, Principal},
};
pub const ANCHOR_NEXT_STEP: &str = "re-pair or recover";
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PinSource {
    Pairing,
    Recovery,
    Creator,
}
#[derive(Debug, Clone)]
pub struct TrustAnchor {
    pub(super) account: String,
    pub(super) genesis_digest: String,
}
impl TrustAnchor {
    pub fn account(&self) -> &str {
        &self.account
    }
    pub fn genesis_digest(&self) -> &str {
        &self.genesis_digest
    }
}
#[derive(Debug, Clone)]
pub struct PinCandidate {
    pub(super) account: String,
    pub(super) genesis_digest: String,
    pub(super) pinned_by: PinSource,
    pub(super) seq: u64,
    pub(super) keyring_link: String,
}
impl PinCandidate {
    pub fn account(&self) -> &str {
        &self.account
    }
    pub fn genesis_digest(&self) -> &str {
        &self.genesis_digest
    }
}
#[derive(Debug, Clone)]
enum BootstrapEvidence {
    Creator {
        self_id: String,
    },
    Pairing {
        own: Principal,
        approver: PublicKeys,
        joining_nonce: String,
        approver_nonce: String,
    },
    Recovery {
        credential_id: String,
        keys: PublicKeys,
    },
}
#[derive(Debug, Clone)]
pub struct BootstrapAnchor {
    account: String,
    evidence: BootstrapEvidence,
}
#[derive(Debug, Clone)]
pub enum ChainAnchor {
    Pinned(TrustAnchor),
    Bootstrap(BootstrapAnchor),
}
impl ChainAnchor {
    pub fn account(&self) -> &str {
        match self {
            Self::Pinned(a) => &a.account,
            Self::Bootstrap(a) => &a.account,
        }
    }
}
#[derive(Debug, Clone)]
pub struct PublicKeys {
    pub sealing_public_key: String,
    pub signing_public_key: String,
}
pub fn creator_anchor(account: String, keyring: &dyn DeviceKeyring) -> BootstrapAnchor {
    BootstrapAnchor {
        account,
        evidence: BootstrapEvidence::Creator {
            self_id: keyring.principal().id,
        },
    }
}
pub fn pairing_anchor(
    account: String,
    keyring: &dyn DeviceKeyring,
    approver: PublicKeys,
    joining_nonce: String,
    approver_nonce: String,
) -> BootstrapAnchor {
    let keys = keyring.public_keys();
    BootstrapAnchor {
        account,
        evidence: BootstrapEvidence::Pairing {
            own: Principal {
                kind: "device".into(),
                id: keyring.principal().id,
                sealing_public_key: keys.sealing,
                signing_public_key: keys.signing,
            },
            approver,
            joining_nonce,
            approver_nonce,
        },
    }
}
pub fn recovery_anchor(
    account: String,
    credential_id: String,
    keys: PublicKeys,
) -> BootstrapAnchor {
    BootstrapAnchor {
        account,
        evidence: BootstrapEvidence::Recovery {
            credential_id,
            keys,
        },
    }
}
fn candidate(
    anchor: &BootstrapAnchor,
    digest: &str,
    source: PinSource,
    entry: &KeyringEntry,
) -> Option<PinCandidate> {
    Some(PinCandidate {
        account: anchor.account.clone(),
        genesis_digest: digest.into(),
        pinned_by: source,
        seq: entry.seq,
        keyring_link: entry.fields.keyring_link.clone()?,
    })
}
pub(super) fn anchor_step(
    anchor: &ChainAnchor,
    entry: &KeyringEntry,
    body: &ParsedBody,
    before: Option<&Authority>,
    genesis: &str,
) -> Result<(bool, Option<PinCandidate>), &'static str> {
    match anchor {
        ChainAnchor::Pinned(a) => {
            if !matches!(body, ParsedBody::Genesis { .. }) {
                return Ok((false, None));
            }
            if entry.fields.digest != a.genesis_digest {
                return Err("untrusted_root");
            }
            Ok((true, None))
        }
        ChainAnchor::Bootstrap(a) => match &a.evidence {
            BootstrapEvidence::Creator { self_id } => {
                let ParsedBody::Genesis { first_device, .. } = body else {
                    return Ok((false, None));
                };
                if first_device.id != *self_id {
                    return Err("anchor_mismatch");
                }
                Ok((
                    true,
                    candidate(a, &entry.fields.digest, PinSource::Creator, entry),
                ))
            }
            BootstrapEvidence::Pairing {
                own,
                approver,
                joining_nonce,
                approver_nonce,
            } => {
                let ParsedBody::Approval { principal, .. } = body else {
                    return Ok((false, None));
                };
                if principal.id != own.id {
                    return Ok((false, None));
                }
                let signer = match &entry.author {
                    super::entry::SyncAuthor::Device { id, .. } => {
                        before.and_then(|a| a.members.get(id))
                    }
                    super::entry::SyncAuthor::Recovery { .. } => None,
                };
                if principal.sealing_public_key != own.sealing_public_key
                    || principal.signing_public_key != own.signing_public_key
                    || !signer.is_some_and(|p| {
                        p.sealing_public_key == approver.sealing_public_key
                            && p.signing_public_key == approver.signing_public_key
                    })
                    || entry.body["pairing"]["joining_nonce"].as_str() != Some(joining_nonce)
                    || entry.body["pairing"]["approver_nonce"].as_str() != Some(approver_nonce)
                {
                    return Err("anchor_mismatch");
                }
                Ok((true, candidate(a, genesis, PinSource::Pairing, entry)))
            }
            BootstrapEvidence::Recovery {
                credential_id,
                keys,
            } => {
                let enrolled = match body {
                    ParsedBody::Genesis { recovery_key, .. } => recovery_key,
                    ParsedBody::RecoveryKey { key, .. } => key,
                    _ => return Ok((false, None)),
                };
                if enrolled.credential_id != *credential_id {
                    return Ok((false, None));
                }
                if enrolled.signing_public_key != keys.signing_public_key
                    || enrolled.sealing_public_key != keys.sealing_public_key
                {
                    return Err("anchor_mismatch");
                }
                Ok((true, candidate(a, genesis, PinSource::Recovery, entry)))
            }
        },
    }
}
