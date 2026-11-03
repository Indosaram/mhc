//! Port of `desktop-sync/framing.ts`: newline-delimited JSON frames over the private duplex.
//!
//! Binary values travel as `{"$omoSyncBytes": "<canonical base64url>"}`. A packet is a
//! `serde_json::Value`, so bytes stay in that tagged form: [`binary_value`] builds one and
//! [`binary_bytes`] reads one back; the reader rejects a tagged object whose payload is not
//! canonical base64url.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Map, Value};

use super::protocol::DesktopSyncUnavailable;

/// Matches the private desktop wire: ciphertext plus canonical base64url overhead.
pub const MAX_FRAME_BYTES: usize = 92 * 1024 * 1024;
pub const BINARY_TAG: &str = "$omoSyncBytes";

/// `{ "$omoSyncBytes": base64url(bytes) }`.
pub fn binary_value(bytes: &[u8]) -> Value {
    let mut object = Map::new();
    object.insert(BINARY_TAG.into(), URL_SAFE_NO_PAD.encode(bytes).into());
    Value::Object(object)
}

/// The bytes of a tagged binary value, or `None` for any other value.
pub fn binary_bytes(value: &Value) -> Option<Vec<u8>> {
    let object = value.as_object().filter(|object| object.len() == 1)?;
    URL_SAFE_NO_PAD
        .decode(object.get(BINARY_TAG)?.as_str()?)
        .ok()
}

/// One frame: the packet's JSON plus a newline. Errors when the frame exceeds the limit.
pub fn encode_frame(packet: &Value) -> Result<Vec<u8>, DesktopSyncUnavailable> {
    let mut line = serde_json::to_vec(packet).map_err(|_| DesktopSyncUnavailable)?;
    if line.len() > MAX_FRAME_BYTES {
        return Err(DesktopSyncUnavailable);
    }
    line.push(b'\n');
    Ok(line)
}

/// Every tagged binary object in the value must carry canonical base64url.
fn validate_binary(value: &Value) -> Result<(), DesktopSyncUnavailable> {
    match value {
        Value::Object(object) => {
            if object.len() == 1
                && let Some(encoded) = object.get(BINARY_TAG)
            {
                let encoded = encoded.as_str().ok_or(DesktopSyncUnavailable)?;
                let canonical = encoded
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
                let bytes = URL_SAFE_NO_PAD
                    .decode(encoded)
                    .map_err(|_| DesktopSyncUnavailable)?;
                if !canonical || URL_SAFE_NO_PAD.encode(bytes) != encoded {
                    return Err(DesktopSyncUnavailable);
                }
                return Ok(());
            }
            object.values().try_for_each(validate_binary)
        }
        Value::Array(items) => items.iter().try_for_each(validate_binary),
        _ => Ok(()),
    }
}

/// Incremental reader: feed chunks, get whole packets. A frame over the limit, malformed JSON or a
/// non-canonical binary payload is an error; the caller closes the channel (an untrusted framing
/// boundary: neither parser errors nor input bytes leave the channel).
#[derive(Default)]
pub struct FrameReader {
    pending: Vec<u8>,
}

impl FrameReader {
    pub fn push(
        &mut self,
        chunk: &[u8],
        mut receive: impl FnMut(Value) -> bool,
    ) -> Result<(), DesktopSyncUnavailable> {
        // Consume a chunk a frame at a time so even one very large write cannot bypass the buffer limit.
        let mut offset = 0;
        while offset < chunk.len() {
            let newline = chunk[offset..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|at| at + offset);
            let end = newline.unwrap_or(chunk.len());
            if self.pending.len() + end - offset > MAX_FRAME_BYTES {
                return Err(DesktopSyncUnavailable);
            }
            self.pending.extend_from_slice(&chunk[offset..end]);
            let Some(newline) = newline else {
                return Ok(());
            };
            let value: Value =
                serde_json::from_slice(&self.pending).map_err(|_| DesktopSyncUnavailable)?;
            validate_binary(&value)?;
            self.pending.clear();
            if !receive(value) {
                return Ok(());
            }
            offset = newline + 1;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn frames_round_trip_across_arbitrary_chunk_boundaries() {
        let packet = json!({"id": 1, "payload": binary_value(&[1, 2, 3, 255])});
        let bytes = encode_frame(&packet).unwrap();
        assert_eq!(*bytes.last().unwrap(), b'\n');
        let mut reader = FrameReader::default();
        let mut received = Vec::new();
        for chunk in bytes.chunks(3) {
            reader
                .push(chunk, |value| {
                    received.push(value);
                    true
                })
                .unwrap();
        }
        assert_eq!(received, vec![packet.clone()]);
        assert_eq!(
            binary_bytes(&received[0]["payload"]),
            Some(vec![1, 2, 3, 255])
        );
    }

    #[test]
    fn two_frames_in_one_chunk_are_both_delivered() {
        let mut reader = FrameReader::default();
        let mut count = 0;
        reader
            .push(b"{\"a\":1}\n{\"b\":2}\n", |_| {
                count += 1;
                true
            })
            .unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn non_canonical_binary_and_garbage_are_refused() {
        let mut reader = FrameReader::default();
        assert!(
            reader
                .push(b"{\"x\":{\"$omoSyncBytes\":\"AB==\"}}\n", |_| true)
                .is_err()
        );
        let mut reader = FrameReader::default();
        assert!(
            reader
                .push(b"{\"x\":{\"$omoSyncBytes\":\"AQ\"}}\n", |_| true)
                .is_ok()
        );
        let mut reader = FrameReader::default();
        assert!(reader.push(b"not json\n", |_| true).is_err());
        let mut reader = FrameReader::default();
        assert!(
            reader
                .push(b"{\"x\":{\"$omoSyncBytes\":\"A\"}}\n", |_| true)
                .is_err()
        );
    }
}
