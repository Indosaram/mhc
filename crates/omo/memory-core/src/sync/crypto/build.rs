use super::super::keyring_port::{DeviceKeyring, DeviceKeyringError, DeviceKeyringSignRequest};
use super::{
    entry::SyncRecordFields,
    signatures::*,
    vendor::sync_format::{
        keys::{hkdf_sha256, seal},
        object_types::*,
        stream::{SEGMENT, segment_nonce},
    },
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use flate2::{Compression, write::DeflateEncoder};
use std::io::Write;
pub type SyncPayloadRecord = SyncObjectRecord;
pub struct BuildBatchInput<'a> {
    pub account: &'a str,
    pub kind: ObjectKind,
    pub epoch: u64,
    pub epoch_key: &'a [u8],
    pub keyring: &'a dyn DeviceKeyring,
    pub peers: &'a [String],
    pub batch_id: &'a [u8],
    pub salt: &'a [u8],
    pub created_at: u64,
    pub chunk: Option<Chunk>,
    pub records: &'a [SyncPayloadRecord],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchInputProblem {
    SpaceIdLength,
    ItemKeyTooLong,
    BodyTooLong,
}
impl BatchInputProblem {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SpaceIdLength => "space_id_length",
            Self::ItemKeyTooLong => "item_key_too_long",
            Self::BodyTooLong => "body_too_long",
        }
    }
}
#[derive(Debug)]
pub enum BatchInputError {
    Framing(BatchInputProblem),
    Canonical(super::super::canonical_json::CanonicalJsonError),
    Compression(std::io::Error),
    Key,
    Signing(DeviceKeyringError),
    HeaderLength,
}
impl std::fmt::Display for BatchInputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "buildBatch: {self:?}")
    }
}
impl std::error::Error for BatchInputError {}
fn encode_payload(records: &[SyncPayloadRecord]) -> Result<Vec<u8>, BatchInputError> {
    let mut bytes = Vec::new();
    for record in records {
        if record.space_id.len() != 16 {
            return Err(BatchInputError::Framing(BatchInputProblem::SpaceIdLength));
        }
        let key = record.item_key.as_bytes();
        let key_len = u16::try_from(key.len())
            .map_err(|_| BatchInputError::Framing(BatchInputProblem::ItemKeyTooLong))?;
        let body_len = u32::try_from(record.body.len())
            .map_err(|_| BatchInputError::Framing(BatchInputProblem::BodyTooLong))?;
        bytes.push(record.r#type);
        bytes.extend(&record.space_id);
        bytes.extend(key_len.to_be_bytes());
        bytes.extend(key);
        bytes.extend(body_len.to_be_bytes());
        bytes.extend(&record.body);
    }
    Ok(bytes)
}
pub fn build_batch(input: &BuildBatchInput<'_>) -> Result<Vec<u8>, BatchInputError> {
    let principal = input.keyring.principal();
    let header = SyncObjectHeader {
        account_id: input.account.into(),
        batch_id: URL_SAFE_NO_PAD.encode(input.batch_id),
        kind: input.kind,
        epoch: input.epoch,
        author: ObjectAuthor {
            kind: principal.kind.into(),
            id: principal.id,
        },
        peers: input.peers.to_vec(),
        salt: URL_SAFE_NO_PAD.encode(input.salt),
        created_at: input.created_at,
        chunk: input.chunk.clone(),
    };
    let header_bytes = canonical(&serde_json::json!(header)).map_err(BatchInputError::Canonical)?;
    let key = hkdf_sha256(input.epoch_key, input.salt, b"omo-sync/v1/batch");
    let payload = encode_payload(input.records)?;
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
    encoder
        .write_all(&payload)
        .map_err(BatchInputError::Compression)?;
    let compressed = encoder.finish().map_err(BatchInputError::Compression)?;
    let count = compressed.len().div_ceil(SEGMENT).max(1);
    let mut ciphertext = Vec::new();
    let aad = sha256(&header_bytes);
    for index in 0..count {
        let start = index * SEGMENT;
        let end = ((index + 1) * SEGMENT).min(compressed.len());
        ciphertext.extend(
            seal(
                &key,
                segment_nonce(
                    u64::try_from(index).map_err(|_| BatchInputError::Key)?,
                    index + 1 == count,
                ),
                &aad,
                &compressed[start..end],
            )
            .ok_or(BatchInputError::Key)?,
        );
    }
    let mut unsigned = b"OMOB\x01\x00".to_vec();
    unsigned.extend(
        u32::try_from(header_bytes.len())
            .map_err(|_| BatchInputError::HeaderLength)?
            .to_be_bytes(),
    );
    unsigned.extend(header_bytes);
    unsigned.extend(ciphertext);
    let sig = input
        .keyring
        .sign(DeviceKeyringSignRequest::ObjectDigest {
            digest: &sha256(&unsigned),
        })
        .map_err(BatchInputError::Signing)?;
    unsigned.extend(sig);
    Ok(unsigned)
}
pub fn sign_record(
    keyring: &dyn DeviceKeyring,
    fields: &SyncRecordFields,
) -> Result<String, BatchInputError> {
    let bytes = canonical(&record_payload(fields)).map_err(BatchInputError::Canonical)?;
    Ok(URL_SAFE_NO_PAD.encode(
        keyring
            .sign(DeviceKeyringSignRequest::Record {
                canonical_json: &bytes,
            })
            .map_err(BatchInputError::Signing)?,
    ))
}
