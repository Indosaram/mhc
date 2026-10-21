use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectKind {
    Ops,
    Snapshot,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    pub index: u64,
    pub count: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObjectAuthor {
    pub kind: String,
    pub id: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncObjectHeader {
    pub account_id: String,
    pub batch_id: String,
    pub kind: ObjectKind,
    pub epoch: u64,
    pub author: ObjectAuthor,
    pub peers: Vec<String>,
    pub salt: String,
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<Chunk>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncObjectRecord {
    pub r#type: u8,
    pub space_id: Vec<u8>,
    pub item_key: String,
    pub body: Vec<u8>,
}
pub type ObjectFailureReason = &'static str;
pub struct ObjectContext<'a> {
    pub account: &'a str,
    pub epoch: u64,
    pub author_id: &'a str,
    pub author_signing_public_key: &'a [u8],
}
pub struct ParsedFrame<'a> {
    pub header: SyncObjectHeader,
    pub header_bytes: &'a [u8],
    pub ciphertext: &'a [u8],
    pub signature: &'a [u8],
}
