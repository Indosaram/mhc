use super::{
    ItemError,
    document::{ItemFrontier, item_frontier},
};
use crate::support::{random::random_id, sha256::sha256_hex};
use loro::{ID, LoroDoc};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub const MAX_RECEIPTS: usize = 32;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OpSpan {
    pub peer: String,
    pub start: i32,
    pub end: i32,
    #[serde(rename = "captureId")]
    pub capture_id: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CaptureReceipt {
    pub base: ItemFrontier,
    pub text: String,
    pub at: ItemFrontier,
    pub stamps: Vec<u64>,
    pub seen: Vec<String>,
    pub span: Option<OpSpan>,
}
pub fn receipt_path(state_dir: &Path, item_key: &str) -> PathBuf {
    state_dir
        .join("captures")
        .join(format!("{}.json", sha256_hex(item_key.as_bytes())))
}
pub fn read_receipts(state_dir: &Path, item_key: &str) -> Result<Vec<CaptureReceipt>, ItemError> {
    let bytes = match fs::read(receipt_path(state_dir, item_key)) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };
    let raw: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|e| ItemError::Receipt(e.to_string()))?;
    if !raw
        .as_array()
        .is_some_and(|entries| entries.iter().all(|e| e.get("span").is_some()))
    {
        return Err(ItemError::Receipt(format!(
            "malformed capture receipt: {item_key}"
        )));
    }
    let receipts: Vec<CaptureReceipt> =
        serde_json::from_value(raw).map_err(|e| ItemError::Receipt(e.to_string()))?;
    for r in &receipts {
        if r.base.iter().chain(&r.at).any(|id| {
            id.peer.is_empty()
                || !id.peer.bytes().all(|b| b.is_ascii_digit())
                || id.peer.parse::<u64>().is_err()
        }) || r.span.as_ref().is_some_and(|s| {
            s.peer.is_empty()
                || !s.peer.bytes().all(|b| b.is_ascii_digit())
                || s.peer.parse::<u64>().is_err()
        }) {
            return Err(ItemError::Receipt(format!(
                "malformed capture receipt: {item_key}"
            )));
        }
    }
    Ok(receipts)
}
pub fn write_receipts(
    state_dir: &Path,
    item_key: &str,
    receipts: &[CaptureReceipt],
) -> Result<(), ItemError> {
    let path = receipt_path(state_dir, item_key);
    fs::create_dir_all(state_dir.join("captures"))?;
    let temp = path.with_extension(format!("{}.{}.tmp", std::process::id(), random_id()));
    let bytes = serde_json::to_vec(&receipts[receipts.len().saturating_sub(MAX_RECEIPTS)..])
        .map_err(|e| ItemError::Receipt(e.to_string()))?;
    super::device_peer::write_private(&temp, &bytes)?;
    fs::rename(temp, path)?;
    Ok(())
}
pub fn same_frontier(a: &ItemFrontier, b: &ItemFrontier) -> bool {
    a.len() == b.len() && a.iter().all(|id| b.contains(id))
}
pub fn stamps_of(doc: &LoroDoc, frontier: &ItemFrontier) -> Result<Vec<u64>, ItemError> {
    frontier
        .iter()
        .map(|id| {
            let peer = id
                .peer
                .parse::<u64>()
                .map_err(|e| ItemError::Receipt(e.to_string()))?;
            let change = doc
                .get_change(ID::new(peer, id.counter))
                .ok_or_else(|| ItemError::Receipt("receipt frontier operation missing".into()))?;
            let offset = u64::try_from(id.counter - change.id.counter)
                .map_err(|e| ItemError::Receipt(e.to_string()))?;
            Ok(u64::from(change.lamport) + offset)
        })
        .collect()
}
pub fn receipt_landed(doc: &LoroDoc, receipt: &CaptureReceipt) -> bool {
    let version = doc.oplog_vv();
    for id in &receipt.at {
        let Ok(peer) = id.peer.parse::<u64>() else {
            return false;
        };
        if version.get(&peer).copied().unwrap_or(0) <= id.counter {
            return false;
        }
    }
    if !stamps_of(doc, &receipt.at).is_ok_and(|s| s == receipt.stamps) {
        return false;
    }
    let Some(span) = &receipt.span else {
        return true;
    };
    let Ok(peer) = span.peer.parse::<u64>() else {
        return false;
    };
    if version.get(&peer).copied().unwrap_or(0) < span.end {
        return false;
    }
    doc.get_change(ID::new(peer, span.start))
        .is_some_and(|change| {
            change.id.counter == span.start
                && change.message.as_deref() == Some(capture_message(&span.capture_id).as_str())
        })
}
pub fn capture_message(id: &str) -> String {
    format!("omo-capture:{id}")
}
pub(crate) fn receipt_for(
    doc: &LoroDoc,
    base: &ItemFrontier,
    text: &str,
    seen: Vec<String>,
    span: Option<OpSpan>,
) -> Result<CaptureReceipt, ItemError> {
    let at = item_frontier(doc);
    let stamps = stamps_of(doc, &at)?;
    Ok(CaptureReceipt {
        base: base.clone(),
        text: text.into(),
        at,
        stamps,
        seen,
        span,
    })
}
