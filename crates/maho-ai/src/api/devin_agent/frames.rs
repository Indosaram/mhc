//! Port of senpi packages/ai/src/api/devin-agent/frames.ts.
//!
//! Connect streaming frame codec. Each frame is a 5-byte prefix - one flag byte plus a big-endian
//! payload length - followed by the payload. Requests are gzipped (flag 0x01); the final response
//! frame carries JSON trailers (flag 0x02) instead of a message.

use std::io::Write;

use bytes::Bytes;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use futures::{Stream, StreamExt};
use prost::Message;

use crate::api::devin_agent::paths::{DEVIN_COMPRESSED_FLAG, DEVIN_MAX_FRAME_PAYLOAD, DEVIN_TRAILER_FLAG};

/// `DevinFrame`: one decoded frame - a message, or the raw end-of-stream trailer JSON.
#[derive(Debug, Clone, PartialEq)]
pub enum DevinFrame<M> {
    Message(M),
    Trailer(String),
}

/// A malformed frame stream or a rejection the caller raised while consuming one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DevinFrameError {
    #[error("Devin Connect frame length {length} exceeds the {DEVIN_MAX_FRAME_PAYLOAD}-byte cap")]
    FrameTooLarge { length: u32 },
    #[error("Devin Connect frame payload is not a {schema} message: {message}")]
    Decode { schema: &'static str, message: String },
    #[error("Devin Connect frame payload is not gzip: {message}")]
    Gunzip { message: String },
    #[error("{0}")]
    Rejected(String),
}

/// `encodeDevinFrame`: encodes one request message as a single gzipped Connect frame.
pub fn encode_devin_frame<M: Message>(message: &M) -> Vec<u8> {
    let payload = gzip(&message.encode_to_vec());
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(DEVIN_COMPRESSED_FLAG);
    frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    frame.extend_from_slice(&payload);
    frame
}

/// `decodeDevinFrames`: decodes a Connect frame stream, handing each frame to `on_frame` in arrival
/// order so callers can map deltas as they land. The trailer frame yields its raw JSON instead of a
/// message. A `on_frame` rejection aborts the decode with the same error, mirroring the TS throw
/// that propagates out of the generator.
pub async fn decode_devin_frames<M, S, F>(body: S, mut on_frame: F) -> Result<(), DevinFrameError>
where
    M: Message + Default,
    S: Stream<Item = Result<Bytes, reqwest::Error>>,
    F: FnMut(DevinFrame<M>) -> Result<(), DevinFrameError>,
{
    let mut pending: Vec<u8> = Vec::new();
    let mut body = std::pin::pin!(body);
    while let Some(chunk) = body.next().await {
        let chunk = chunk.map_err(|error| DevinFrameError::Rejected(error.to_string()))?;
        if !chunk.is_empty() {
            pending.extend_from_slice(&chunk);
        }
        while pending.len() >= 5 {
            let length = u32::from_be_bytes([pending[1], pending[2], pending[3], pending[4]]);
            if length > DEVIN_MAX_FRAME_PAYLOAD {
                return Err(DevinFrameError::FrameTooLarge { length });
            }
            let frame_length = 5 + length as usize;
            if pending.len() < frame_length {
                break;
            }
            let flags = pending[0];
            let raw = pending[5..frame_length].to_vec();
            pending.drain(..frame_length);
            let payload = if flags & DEVIN_COMPRESSED_FLAG == 0 { raw } else { gunzip(&raw)? };
            let frame = if flags & DEVIN_TRAILER_FLAG == 0 {
                DevinFrame::Message(M::decode(payload.as_slice()).map_err(|error| DevinFrameError::Decode {
                    schema: std::any::type_name::<M>(),
                    message: error.to_string(),
                })?)
            } else {
                DevinFrame::Trailer(String::from_utf8_lossy(&payload).into_owned())
            };
            on_frame(frame)?;
        }
    }
    Ok(())
}

fn gzip(payload: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(payload).expect("gzip a Connect frame payload into memory");
    encoder.finish().expect("finish a Connect frame gzip stream")
}

fn gunzip(payload: &[u8]) -> Result<Vec<u8>, DevinFrameError> {
    let mut decoder = GzDecoder::new(payload);
    let mut out = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut out)
        .map_err(|error| DevinFrameError::Gunzip { message: error.to_string() })?;
    Ok(out)
}
