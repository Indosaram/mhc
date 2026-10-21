use super::super::super::signatures::{decode_b64, sha256};
use super::{
    keys::{hkdf_sha256, open},
    object_types::*,
};
use flate2::{Decompress, FlushDecompress, Status};
pub const SEGMENT: usize = 65_536;
pub fn segment_nonce(index: u64, last: bool) -> [u8; 12] {
    let mut nonce = [0; 12];
    nonce[3..11].copy_from_slice(&index.to_be_bytes());
    nonce[11] = u8::from(last);
    nonce
}
fn inflate_within(compressed: &[u8], limit: usize) -> Result<Vec<u8>, ObjectFailureReason> {
    let mut decoder = Decompress::new(false);
    let mut output = Vec::new();
    let mut offset = 0;
    loop {
        let mut block = [0; 8192];
        let old_in = decoder.total_in();
        let old_out = decoder.total_out();
        let status = decoder
            .decompress(&compressed[offset..], &mut block, FlushDecompress::None)
            .map_err(|_| "bad_payload")?;
        let used = usize::try_from(decoder.total_in() - old_in).map_err(|_| "bad_payload")?;
        let written = usize::try_from(decoder.total_out() - old_out).map_err(|_| "bad_payload")?;
        offset += used;
        if output.len().saturating_add(written) > limit {
            return Err("payload_too_large");
        }
        output.extend_from_slice(&block[..written]);
        if status == Status::StreamEnd {
            return if offset == compressed.len() {
                Ok(output)
            } else {
                Err("bad_payload")
            };
        }
        if used == 0 && written == 0 {
            return Err("bad_payload");
        }
    }
}
pub fn stream_open(
    frame: &ParsedFrame<'_>,
    epoch_key: &[u8],
) -> Result<Vec<u8>, ObjectFailureReason> {
    let salt = decode_b64(&frame.header.salt, None).ok_or("bad_header")?;
    let key = hkdf_sha256(epoch_key, &salt, b"omo-sync/v1/batch");
    let aad = sha256(frame.header_bytes);
    let mut compressed = Vec::new();
    let mut offset = 0;
    let mut index = 0;
    loop {
        let remaining = frame.ciphertext.len() - offset;
        if remaining < 17 {
            return Err("truncated");
        }
        let last = remaining <= SEGMENT + 16;
        let length = remaining.min(SEGMENT + 16);
        let cipher = &frame.ciphertext[offset..offset + length];
        let Some(plain) = open(&key, segment_nonce(index, last), &aad, cipher) else {
            let prefix = last
                && length == SEGMENT + 16
                && open(&key, segment_nonce(index, false), &aad, cipher).is_some();
            return Err(if prefix { "truncated" } else { "stream_aead" });
        };
        compressed.extend(plain);
        offset += length;
        index += 1;
        if last {
            break;
        }
    }
    inflate_within(
        &compressed,
        match frame.header.kind {
            ObjectKind::Ops => 8 * 1024 * 1024,
            ObjectKind::Snapshot => 64 * 1024 * 1024,
        },
    )
}
pub fn parse_records(payload: &[u8]) -> Result<Vec<SyncObjectRecord>, ObjectFailureReason> {
    let mut records = Vec::new();
    let mut offset = 0;
    while offset < payload.len() {
        if payload.len() - offset < 23 {
            return Err("bad_payload");
        }
        let kind = payload[offset];
        if !(1..=3).contains(&kind) {
            return Err("bad_payload");
        }
        let key_length = usize::from(u16::from_be_bytes(
            payload[offset + 17..offset + 19]
                .try_into()
                .map_err(|_| "bad_payload")?,
        ));
        let key_start = offset + 19;
        if payload.len() - key_start < key_length + 4 {
            return Err("bad_payload");
        }
        let item_key = std::str::from_utf8(&payload[key_start..key_start + key_length])
            .map_err(|_| "bad_payload")?
            .into();
        let body_length = usize::try_from(u32::from_be_bytes(
            payload[key_start + key_length..key_start + key_length + 4]
                .try_into()
                .map_err(|_| "bad_payload")?,
        ))
        .map_err(|_| "bad_payload")?;
        let start = key_start + key_length + 4;
        if payload.len() - start < body_length {
            return Err("bad_payload");
        }
        records.push(SyncObjectRecord {
            r#type: kind,
            space_id: payload[offset + 1..offset + 17].to_vec(),
            item_key,
            body: payload[start..start + body_length].to_vec(),
        });
        offset = start + body_length;
    }
    Ok(records)
}
