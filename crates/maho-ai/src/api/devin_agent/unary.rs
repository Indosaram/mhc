//! Port of senpi packages/ai/src/api/devin-agent/unary.ts.
//!
//! Unary Cascade RPCs (`GetUserJwt`, `AssignModel`, `GetCliModelConfigs`). Unlike the streaming chat
//! call these carry a bare protobuf body both ways. Edges variously answer with plain or gzipped
//! protobuf, so decoding tries the bytes as-is before falling back to gunzip.

use flate2::read::GzDecoder;
use prost::Message;

use crate::api::devin_agent::paths::DEVIN_UNARY_HEADERS;
use crate::utils::abort::{race_with_abort_signal, AbortSignal};

const MAX_UNARY_ERROR_BODY_CHARS: usize = 500;

/// `DevinUnaryError`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct DevinUnaryError {
    pub rpc: String,
    pub status: u16,
    pub detail: String,
}

impl std::fmt::Display for DevinUnaryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "Devin {} failed (HTTP {})", self.rpc, self.status)?;
        if !self.detail.is_empty() {
            write!(formatter, ": {}", self.detail)?;
        }
        Ok(())
    }
}

/// `DevinUnaryInput`: one unary RPC against the account's Cascade edge.
pub struct DevinUnaryInput<'a, Req: Message> {
    pub client: &'a reqwest::Client,
    pub base_url: &'a str,
    pub path: &'a str,
    pub request: &'a Req,
    pub signal: Option<&'a AbortSignal>,
}

/// `postDevinUnary`.
pub async fn post_devin_unary<Req, Res>(input: DevinUnaryInput<'_, Req>) -> Result<Res, DevinUnaryError>
where
    Req: Message,
    Res: Message + Default,
{
    let mut request = input.client.post(format!("{}{}", input.base_url, input.path));
    for (name, value) in DEVIN_UNARY_HEADERS {
        request = request.header(name, value);
    }
    let send = request.body(input.request.encode_to_vec()).send();
    let response = match input.signal {
        Some(signal) => race_with_abort_signal(send, signal)
            .await
            .map_err(|_| DevinUnaryError {
                rpc: rpc_name(input.path),
                status: 0,
                detail: String::from("Request was aborted"),
            })?,
        None => send.await,
    }
    .map_err(|error| DevinUnaryError { rpc: rpc_name(input.path), status: 0, detail: error.to_string() })?;
    let status = response.status().as_u16();
    let payload = response.bytes().await.map_err(|error| DevinUnaryError {
        rpc: rpc_name(input.path),
        status,
        detail: error.to_string(),
    })?;
    if !(200..300).contains(&status) {
        return Err(DevinUnaryError {
            rpc: rpc_name(input.path),
            status,
            detail: slice_utf16(&String::from_utf8_lossy(&payload), MAX_UNARY_ERROR_BODY_CHARS),
        });
    }
    match decode_devin_unary::<Res>(&payload) {
        Some(decoded) => Ok(decoded),
        None => Err(DevinUnaryError {
            rpc: rpc_name(input.path),
            status,
            detail: String::from("response is not a protobuf message"),
        }),
    }
}

/// `decodeDevinUnary`: the bytes as-is, then gunzipped, then `None`.
pub fn decode_devin_unary<Res: Message + Default>(payload: &[u8]) -> Option<Res> {
    if let Ok(decoded) = Res::decode(payload) {
        return Some(decoded);
    }
    let mut decoder = GzDecoder::new(payload);
    let mut inflated = Vec::new();
    if std::io::Read::read_to_end(&mut decoder, &mut inflated).is_err() {
        return None;
    }
    Res::decode(inflated.as_slice()).ok()
}

fn rpc_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or_default().to_owned()
}

/// `String.prototype.slice(0, max)`: the first `max` UTF-16 code units.
fn slice_utf16(text: &str, max_units: usize) -> String {
    let mut units = 0usize;
    let mut out = String::new();
    for character in text.chars() {
        let width = character.len_utf16();
        if units + width > max_units {
            break;
        }
        units += width;
        out.push(character);
    }
    out
}
