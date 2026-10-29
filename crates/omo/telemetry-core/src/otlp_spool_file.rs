//! Self-contained length-prefixed OTLP envelopes. Attribution always comes from the file.
use std::{fmt, fs, io, path::Path};
pub const SPOOL_FORMAT: u8 = 1;
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Signal {
    Logs,
    Traces,
}
impl fmt::Display for Signal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Logs => "logs",
            Self::Traces => "traces",
        })
    }
}
pub fn frame(payload: &[u8]) -> Vec<u8> {
    let mut out = (payload.len() as u32).to_le_bytes().to_vec();
    out.extend_from_slice(payload);
    out
}
/// Resource and scope are their protobuf messages, not a replayer's current resource.
pub fn encode_header(signal: Signal, resource: &[u8], scope: &[u8]) -> Vec<u8> {
    let mut out = vec![
        SPOOL_FORMAT,
        match signal {
            Signal::Logs => 1,
            Signal::Traces => 2,
        },
    ];
    field(&mut out, 1, resource);
    let mut scoped = Vec::new();
    field(&mut scoped, 1, scope);
    field(&mut out, 2, &scoped);
    out
}
pub fn read_frames(bytes: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut at = 0;
    while bytes.len().saturating_sub(at) >= 4 {
        let length =
            u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]) as usize;
        at += 4;
        if length > bytes.len() - at {
            break;
        }
        frames.push(&bytes[at..at + length]);
        at += length;
    }
    frames
}
#[derive(Debug, PartialEq, Eq)]
pub enum SpoolRequest {
    Ok {
        signal: Signal,
        records: usize,
        body: Vec<u8>,
    },
    Corrupt {
        records: usize,
    },
}
fn header_signal(header: &[u8]) -> Option<Signal> {
    if header.first() != Some(&SPOOL_FORMAT) {
        return None;
    }
    match header.get(1) {
        Some(1) => Some(Signal::Logs),
        Some(2) => Some(Signal::Traces),
        _ => None,
    }
}
fn looks_like_header(header: &[u8]) -> bool {
    matches!(header.get(1), Some(1 | 2))
}
pub(crate) fn varint(out: &mut Vec<u8>, mut value: u64) {
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
pub(crate) fn field(out: &mut Vec<u8>, number: u64, bytes: &[u8]) {
    varint(out, number * 8 + 2);
    varint(out, bytes.len() as u64);
    out.extend_from_slice(bytes);
}
fn read_varint(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let b = *bytes.get(*at)?;
        *at += 1;
        if shift == 63 && b > 1 {
            return None;
        }
        value |= u64::from(b & 127) << shift;
        if b & 128 == 0 {
            return Some(value);
        }
    }
    None
}
type WireField<'a> = (u64, u8, &'a [u8], &'a [u8]);
/// Decode wire fields while retaining unknown fields verbatim for forward compatibility.
fn fields(bytes: &[u8]) -> Option<Vec<WireField<'_>>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        let start = at;
        let tag = read_varint(bytes, &mut at)?;
        if tag >> 3 == 0 {
            return None;
        }
        let wire = (tag & 7) as u8;
        let data = match wire {
            0 => {
                let p = at;
                read_varint(bytes, &mut at)?;
                bytes.get(p..at)?
            }
            1 | 5 => {
                let p = at;
                at = at.checked_add(if wire == 1 { 8 } else { 4 })?;
                bytes.get(p..at)?
            }
            2 => {
                let len = usize::try_from(read_varint(bytes, &mut at)?).ok()?;
                let p = at;
                at = at.checked_add(len)?;
                bytes.get(p..at)?
            }
            _ => return None,
        };
        out.push((tag >> 3, wire, data, bytes.get(start..at)?));
    }
    Some(out)
}
fn build_body(header: &[u8], records: &[&[u8]]) -> Option<Vec<u8>> {
    let parsed = fields(header)?;
    let scope = parsed.iter().find(|(n, w, _, _)| *n == 2 && *w == 2)?.2;
    let mut scoped = Vec::new();
    for (n, _, _, raw) in fields(scope)? {
        if n != 2 {
            scoped.extend_from_slice(raw);
        }
    }
    for record in records {
        fields(record)?;
        field(&mut scoped, 2, record);
    }
    let mut resource = Vec::new();
    let mut replaced = false;
    for (n, w, _, raw) in parsed {
        if n == 2 && w == 2 && !replaced {
            field(&mut resource, 2, &scoped);
            replaced = true;
        } else {
            resource.extend_from_slice(raw);
        }
    }
    let mut body = Vec::new();
    field(&mut body, 1, &resource);
    Some(body)
}
pub fn spool_request(bytes: &[u8]) -> SpoolRequest {
    let frames = read_frames(bytes);
    let Some(first) = frames.first() else {
        return SpoolRequest::Corrupt { records: 0 };
    };
    let records = frames.len() - 1;
    let Some(signal) = header_signal(first) else {
        return SpoolRequest::Corrupt {
            records: if looks_like_header(first) {
                records
            } else {
                frames.len()
            },
        };
    };
    match build_body(&first[2..], &frames[1..]) {
        Some(body) => SpoolRequest::Ok {
            signal,
            records,
            body,
        },
        None => SpoolRequest::Corrupt { records },
    }
}
pub fn read_spool_request(path: &Path) -> io::Result<SpoolRequest> {
    Ok(spool_request(&fs::read(path)?))
}
pub fn count_spool_records(path: &Path) -> usize {
    let Ok(bytes) = fs::read(path) else {
        return 0;
    };
    let frames = read_frames(&bytes);
    frames.len().saturating_sub(usize::from(
        frames.first().is_some_and(|f| looks_like_header(f)),
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn torn_tail_is_not_a_record() {
        let mut bytes = frame(&encode_header(Signal::Logs, &[], &[]));
        bytes.extend(frame(&[]));
        bytes.extend([9, 0, 0, 0, 1]);
        assert_eq!(read_frames(&bytes).len(), 2);
        assert!(matches!(
            spool_request(&bytes),
            SpoolRequest::Ok {
                records: 1,
                signal: Signal::Logs,
                ..
            }
        ));
    }
    #[test]
    fn reconstructs_both_signals_and_rejects_bad_headers() {
        for signal in [Signal::Logs, Signal::Traces] {
            let mut bytes = frame(&encode_header(signal, &[10, 0], &[]));
            bytes.extend(frame(&[8, 1]));
            assert!(matches!(
                spool_request(&bytes),
                SpoolRequest::Ok { records: 1, .. }
            ));
        }
        assert_eq!(
            spool_request(&frame(&[9, 1])),
            SpoolRequest::Corrupt { records: 0 }
        );
        assert_eq!(
            spool_request(&frame(&[1, 1, 255])),
            SpoolRequest::Corrupt { records: 0 }
        );
        assert_eq!(
            spool_request(&frame(&[8, 1])),
            SpoolRequest::Corrupt { records: 0 }
        );
    }
}
