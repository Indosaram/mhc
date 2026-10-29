use super::super::{
    canonical_json::require_canonical_json,
    keyring_port::{DeviceKeyring, WrapBinding},
};
use super::{entry::VerifiedWrap, signatures::*, vendor::sync_format::keys::open};
use serde_json::Value;
use std::collections::BTreeMap;
#[derive(Debug)]
pub struct EpochKeys {
    pub epoch: u64,
    pub current: Vec<u8>,
    pub history: BTreeMap<u64, Vec<u8>>,
}
pub type WrapFailure = &'static str;
pub type OpenedHistory = (Vec<u8>, BTreeMap<u64, Vec<u8>>);
pub const UNVERIFIED_WRAP: WrapFailure = "unverified_wrap";
#[derive(Debug)]
pub enum WrapOutcome {
    Opened { keys: EpochKeys },
    Rejected { reason: WrapFailure },
}
pub fn open_unverified_wrap(_wrap: &super::entry::WrapEntry) -> WrapOutcome {
    WrapOutcome::Rejected {
        reason: UNVERIFIED_WRAP,
    }
}
pub fn open_history(
    material: &Value,
    account: &str,
    epoch: u64,
    id: &str,
    expected: &BTreeMap<u64, String>,
) -> Option<OpenedHistory> {
    if !material.is_object() || material["version"] != 1 || !material["history"].is_object() {
        return None;
    }
    let history = &material["history"];
    let current = decode_b64(material["current_key"].as_str()?, Some(32))?;
    let nonce: [u8; 12] = decode_b64(history["nonce"].as_str()?, Some(12))?
        .try_into()
        .ok()?;
    let cipher = decode_b64(history["ciphertext"].as_str()?, None)?;
    let aad = canonical(&serde_json::json!({"account":account,"epoch":epoch,"id":id,"version":1}))
        .ok()?;
    let plain = open(&current, nonce, &aad, &cipher)?;
    let rows = require_canonical_json(&plain).ok()?;
    let rows = rows.as_array()?;
    if rows.len() != expected.len() {
        return None;
    }
    let mut keys = BTreeMap::new();
    let mut previous: Option<(u64, String)> = None;
    for row in rows {
        let epoch = row["epoch"].as_u64()?;
        let origin = row["origin"].as_str()?;
        let key = decode_b64(row["key"].as_str()?, Some(32))?;
        if expected.get(&epoch).map(String::as_str) != Some(origin) || keys.contains_key(&epoch) {
            return None;
        }
        if previous.as_ref().is_some_and(|(e, o)| {
            epoch < *e || (epoch == *e && origin.encode_utf16().cmp(o.encode_utf16()).is_le())
        }) {
            return None;
        }
        previous = Some((epoch, origin.into()));
        keys.insert(epoch, key);
    }
    Some((current, keys))
}
pub fn open_wrap(
    keyring: &dyn DeviceKeyring,
    wrap: &VerifiedWrap,
    account: &str,
    expected_history: &BTreeMap<u64, String>,
) -> WrapOutcome {
    let result = (|| {
        if wrap.account() != account {
            return Err("envelope_mismatch");
        }
        let w = wrap.entry();
        if w.id != format!("device:{}", keyring.principal().id) {
            return Err("not_addressed_to_device");
        }
        let sealed = decode_b64(&w.wrap, None).ok_or("malformed_envelope")?;
        let epoch = w
            .epoch
            .to_string()
            .parse::<f64>()
            .map_err(|_| "malformed_envelope")?;
        let plain = keyring
            .unseal(
                &sealed,
                &WrapBinding {
                    account,
                    epoch,
                    id: &w.id,
                },
            )
            .map_err(|_| "wrap_refused")?;
        let envelope = require_canonical_json(&plain).map_err(|_| "malformed_envelope")?;
        if !super::envelope::exact(
            &envelope,
            &["version", "account", "epoch", "id", "key_material"],
        ) || envelope["version"] != 1
        {
            return Err("malformed_envelope");
        }
        if envelope["account"] != account || envelope["epoch"] != w.epoch || envelope["id"] != w.id
        {
            return Err("envelope_mismatch");
        }
        let (current, history) = open_history(
            &envelope["key_material"],
            account,
            w.epoch,
            &w.id,
            expected_history,
        )
        .ok_or("bad_history")?;
        Ok(EpochKeys {
            epoch: w.epoch,
            current,
            history,
        })
    })();
    match result {
        Ok(keys) => WrapOutcome::Opened { keys },
        Err(reason) => WrapOutcome::Rejected { reason },
    }
}
