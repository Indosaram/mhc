use super::{RecoveryError, derive::random, hpke_seal::seal_hpke_box};
use crate::sync::{
    canonical_json::require_canonical_json,
    crypto::{
        entry::WrapEntry, signatures::*, unwrap::open_history, vendor::sync_format::keys::seal,
    },
    hpke_sealed_box::open_hpke_sealed_box,
    keyring_port::{DeviceKeyring, WrapBinding},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::json;
use std::collections::BTreeMap;
use zeroize::Zeroizing;

pub struct HistoryKey {
    pub epoch: u64,
    pub origin: String,
    pub key: Zeroizing<Vec<u8>>,
}
pub struct EpochMaterial {
    pub epoch: u64,
    pub current: Zeroizing<Vec<u8>>,
    pub history: Vec<HistoryKey>,
}
pub struct WrapAddress<'a> {
    pub account: &'a str,
    pub epoch: u64,
    pub id: &'a str,
}
const INFO: &[u8] = b"omo-sync/v1/wrap\0";
fn aad(address: &WrapAddress<'_>) -> Result<Vec<u8>, RecoveryError> {
    Ok(canonical(
        &json!({"account":address.account,"epoch":address.epoch,"id":address.id}),
    )?)
}
pub fn envelope_bytes(
    address: &WrapAddress<'_>,
    material: &EpochMaterial,
) -> Result<Zeroizing<Vec<u8>>, RecoveryError> {
    let mut history: Vec<_> = material.history.iter().collect();
    history.sort_by(|a, b| a.epoch.cmp(&b.epoch).then_with(|| a.origin.cmp(&b.origin)));
    let rows: Vec<_> = history.iter().map(|row| json!({"epoch":row.epoch,"origin":row.origin,"key":URL_SAFE_NO_PAD.encode(&*row.key)})).collect();
    let nonce = random::<12>()?;
    let history_aad = canonical(
        &json!({"account":address.account,"epoch":address.epoch,"id":address.id,"version":1}),
    )?;
    let plaintext = Zeroizing::new(canonical(&json!(rows))?);
    let cipher =
        seal(&material.current, nonce, &history_aad, &plaintext).ok_or(RecoveryError::Crypto)?;
    Ok(Zeroizing::new(canonical(
        &json!({"version":1,"account":address.account,"epoch":address.epoch,"id":address.id,
        "key_material":{"version":1,"current_key":URL_SAFE_NO_PAD.encode(&*material.current),"history":{"nonce":URL_SAFE_NO_PAD.encode(nonce),"ciphertext":URL_SAFE_NO_PAD.encode(cipher)}}}),
    )?))
}
pub fn seal_wrap(
    address: &WrapAddress<'_>,
    public: &str,
    material: &EpochMaterial,
) -> Result<WrapEntry, RecoveryError> {
    let recipient = decode_b64(public, Some(32)).ok_or(RecoveryError::InvalidInput(
        "a recipient sealing key is 32 bytes of base64url",
    ))?;
    let sealed = seal_hpke_box(
        &recipient,
        &envelope_bytes(address, material)?,
        INFO,
        &aad(address)?,
    )?;
    Ok(WrapEntry {
        epoch: address.epoch,
        id: address.id.into(),
        wrap: URL_SAFE_NO_PAD.encode(sealed),
    })
}
#[derive(Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialRefusal {
    WrapRefused,
    MalformedEnvelope,
    EnvelopeMismatch,
    BadHistory,
}
fn parse_material(
    plaintext: &[u8],
    address: &WrapAddress<'_>,
    expected: &BTreeMap<u64, String>,
) -> Result<EpochMaterial, MaterialRefusal> {
    let envelope =
        require_canonical_json(plaintext).map_err(|_| MaterialRefusal::MalformedEnvelope)?;
    let Some(object) = envelope.as_object() else {
        return Err(MaterialRefusal::MalformedEnvelope);
    };
    if object.len() != 5
        || !["version", "account", "epoch", "id", "key_material"]
            .iter()
            .all(|key| object.contains_key(*key))
        || envelope["version"] != 1
    {
        return Err(MaterialRefusal::MalformedEnvelope);
    }
    if envelope["account"] != address.account
        || envelope["epoch"] != address.epoch
        || envelope["id"] != address.id
    {
        return Err(MaterialRefusal::EnvelopeMismatch);
    }
    let (current, history) = open_history(
        &envelope["key_material"],
        address.account,
        address.epoch,
        address.id,
        expected,
    )
    .ok_or(MaterialRefusal::BadHistory)?;
    Ok(EpochMaterial {
        epoch: address.epoch,
        current: Zeroizing::new(current),
        history: history
            .into_iter()
            .map(|(epoch, key)| HistoryKey {
                epoch,
                origin: expected.get(&epoch).cloned().unwrap_or_default(),
                key: Zeroizing::new(key),
            })
            .collect(),
    })
}
pub fn open_wrap_with_key(
    private: &[u8],
    public: &[u8],
    wrap: &WrapEntry,
    address: &WrapAddress<'_>,
    expected: &BTreeMap<u64, String>,
) -> Result<EpochMaterial, MaterialRefusal> {
    if wrap.epoch != address.epoch || wrap.id != address.id {
        return Err(MaterialRefusal::EnvelopeMismatch);
    }
    let sealed = decode_b64(&wrap.wrap, None).ok_or(MaterialRefusal::MalformedEnvelope)?;
    let plaintext = Zeroizing::new(
        open_hpke_sealed_box(
            private,
            public,
            &sealed,
            INFO,
            &aad(address).map_err(|_| MaterialRefusal::MalformedEnvelope)?,
        )
        .map_err(|_| MaterialRefusal::WrapRefused)?,
    );
    parse_material(&plaintext, address, expected)
}
pub fn open_bridge_sealed_wrap(
    keyring: &dyn DeviceKeyring,
    wrap: &WrapEntry,
    address: &WrapAddress<'_>,
    expected: &BTreeMap<u64, String>,
) -> Result<EpochMaterial, MaterialRefusal> {
    if wrap.epoch != address.epoch
        || wrap.id != address.id
        || address.id != format!("device:{}", keyring.principal().id)
    {
        return Err(MaterialRefusal::EnvelopeMismatch);
    }
    let sealed = decode_b64(&wrap.wrap, None).ok_or(MaterialRefusal::MalformedEnvelope)?;
    let epoch = address
        .epoch
        .to_string()
        .parse::<f64>()
        .map_err(|_| MaterialRefusal::MalformedEnvelope)?;
    let plaintext = Zeroizing::new(
        keyring
            .unseal(
                &sealed,
                &WrapBinding {
                    account: address.account,
                    epoch,
                    id: address.id,
                },
            )
            .map_err(|_| MaterialRefusal::WrapRefused)?,
    );
    parse_material(&plaintext, address, expected)
}
pub(super) fn from_opened(
    epoch: u64,
    current: Vec<u8>,
    history: BTreeMap<u64, Vec<u8>>,
    origins: &BTreeMap<u64, String>,
) -> EpochMaterial {
    EpochMaterial {
        epoch,
        current: Zeroizing::new(current),
        history: history
            .into_iter()
            .map(|(epoch, key)| HistoryKey {
                epoch,
                origin: origins.get(&epoch).cloned().unwrap_or_default(),
                key: Zeroizing::new(key),
            })
            .collect(),
    }
}
