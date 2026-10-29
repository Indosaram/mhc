use super::super::super::signatures::{sha256, verify_bytes};
use super::{
    frame::parse_frame,
    object_types::*,
    stream::{parse_records, stream_open},
};
pub fn verify_object(
    bytes: &[u8],
    ctx: &ObjectContext<'_>,
) -> Result<SyncObjectHeader, ObjectFailureReason> {
    let frame = parse_frame(bytes)?;
    if frame.header.account_id != ctx.account {
        return Err("account_mismatch");
    }
    if frame.header.epoch != ctx.epoch {
        return Err("epoch_mismatch");
    }
    if frame.header.author.kind != "device" || frame.header.author.id != ctx.author_id {
        return Err("author_mismatch");
    }
    if !verify_bytes(
        "omo-sync/v1/object-digest",
        &sha256(&bytes[..bytes.len() - 64]),
        ctx.author_signing_public_key,
        frame.signature,
    ) {
        return Err("bad_signature");
    }
    Ok(frame.header)
}
pub fn decode_object(
    bytes: &[u8],
    ctx: &ObjectContext<'_>,
    epoch_key: &[u8],
) -> Result<Vec<SyncObjectRecord>, ObjectFailureReason> {
    verify_object(bytes, ctx)?;
    parse_records(&stream_open(&parse_frame(bytes)?, epoch_key)?)
}
