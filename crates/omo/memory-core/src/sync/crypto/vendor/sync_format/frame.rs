use super::super::super::{envelope::MAX_SAFE_INTEGER, signatures::decode_b64};
use super::object_types::*;
pub fn parse_frame(bytes: &[u8]) -> Result<ParsedFrame<'_>, ObjectFailureReason> {
    if bytes.len() < 92 {
        return Err("truncated");
    }
    if &bytes[..4] != b"OMOB" {
        return Err("bad_magic");
    }
    if bytes[4] != 1 {
        return Err("unsupported_major");
    }
    let length = u32::from_be_bytes(bytes[6..10].try_into().map_err(|_| "truncated")?);
    let end = 10_usize
        .checked_add(usize::try_from(length).map_err(|_| "truncated")?)
        .ok_or("truncated")?;
    if end.checked_add(80).is_none_or(|n| n > bytes.len()) {
        return Err("truncated");
    }
    let header_bytes = &bytes[10..end];
    let raw: serde_json::Value = serde_json::from_slice(header_bytes).map_err(|_| "bad_header")?;
    if raw.get("chunk").is_some_and(|v| v.is_null()) {
        return Err("bad_header");
    }
    let mut normalized = raw.clone();
    for field in ["epoch", "created_at"] {
        normalized[field] = super::super::super::envelope::integer(&raw[field], 0)
            .ok_or("bad_header")?
            .into();
    }
    if raw.get("chunk").is_some() {
        for field in ["index", "count"] {
            normalized["chunk"][field] =
                super::super::super::envelope::integer(&raw["chunk"][field], 0)
                    .ok_or("bad_header")?
                    .into();
        }
    }
    let header: SyncObjectHeader = serde_json::from_value(normalized).map_err(|_| "bad_header")?;
    if header.account_id.is_empty()
        || decode_b64(&header.batch_id, Some(16)).is_none()
        || decode_b64(&header.salt, Some(32)).is_none()
        || header.epoch == 0
        || header.epoch > MAX_SAFE_INTEGER
        || header.created_at > MAX_SAFE_INTEGER
    {
        return Err("bad_header");
    }
    match (&header.kind, &header.chunk) {
        (ObjectKind::Ops, None) => {}
        (ObjectKind::Snapshot, Some(c))
            if c.count > 0 && c.count <= MAX_SAFE_INTEGER && c.index < c.count => {}
        _ => return Err("bad_header"),
    }
    Ok(ParsedFrame {
        header,
        header_bytes,
        ciphertext: &bytes[end..bytes.len() - 64],
        signature: &bytes[bytes.len() - 64..],
    })
}
