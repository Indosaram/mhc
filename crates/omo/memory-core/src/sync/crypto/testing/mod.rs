use super::*;
use crate::sync::keyring_port::*;
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use serde_json::Value;

pub fn fixtures() -> Value {
    serde_json::from_str(include_str!("sync-v1.json")).expect("committed upstream vectors")
}
pub fn bytes(f: &Value, name: &str) -> Vec<u8> {
    STANDARD
        .decode(f["bytes"][name].as_str().expect("fixture object"))
        .expect("base64 fixture")
}
pub fn keyring(f: &Value, name: &str) -> DeviceKeyringImpl {
    let device = &f["json"]["keys.json"]["devices"][name];
    let seed = URL_SAFE_NO_PAD
        .decode(device["signing_seed"].as_str().expect("seed"))
        .expect("seed base64");
    let scalar = URL_SAFE_NO_PAD
        .decode(device["sealing_private"].as_str().expect("scalar"))
        .expect("scalar base64");
    let material = DeviceKeyMaterial {
        signing_private_pkcs8: URL_SAFE_NO_PAD.encode(
            [
                &[
                    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04,
                    0x22, 0x04, 0x20,
                ],
                seed.as_slice(),
            ]
            .concat(),
        ),
        sealing_private_pkcs8: URL_SAFE_NO_PAD.encode(
            [
                &[
                    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04,
                    0x22, 0x04, 0x20,
                ],
                scalar.as_slice(),
            ]
            .concat(),
        ),
    };
    create_device_keyring(
        device["principal"]["id"].as_str().expect("id").into(),
        &material,
    )
    .expect("fixture keyring")
}
pub fn row(f: &Value, name: &str) -> Value {
    let chain = &f["json"]["chain.json"];
    let mut row = chain["records"][name].clone();
    row["wraps"] = chain["wraps"]
        .get(format!("{name}_wraps"))
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    row
}
pub fn main_chain(f: &Value) -> Vec<Value> {
    f["json"]["chain.json"]["main"]
        .as_array()
        .expect("main")
        .iter()
        .map(|n| row(f, n.as_str().expect("name")))
        .collect()
}
pub fn start(f: &Value) -> ChainState {
    let owner = keyring(f, "owner");
    empty_chain(
        "encoding-account".into(),
        ChainAnchor::Bootstrap(creator_anchor("encoding-account".into(), &owner)),
    )
}
pub fn accepted(f: &Value) -> ChainState {
    match append_entries(
        &start(f),
        &main_chain(f),
        &mut create_rejection_counter(None),
    ) {
        AppendOutcome::Accepted { state, .. } => state,
        other => panic!("fixture chain refused: {other:?}"),
    }
}
pub fn epoch_key(f: &Value, epoch: u64) -> Option<Vec<u8>> {
    signatures::decode_b64(
        f["json"]["keys.json"]["epochs"][epoch.to_string()].as_str()?,
        Some(32),
    )
}
