use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", deny_unknown_fields)]
pub enum SyncAuthor {
    #[serde(rename = "device")]
    Device {
        id: String,
        sealing_public_key: String,
        signing_public_key: String,
    },
    #[serde(rename = "recovery")]
    Recovery { credential_id: String },
}

pub type KeyringBody = Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrapEntry {
    pub epoch: u64,
    pub id: String,
    pub wrap: String,
}

/// Private fields prevent structural reconstruction or mutation of an admitted wrap.
#[derive(Debug, Clone)]
pub struct VerifiedWrap {
    pub(super) entry: WrapEntry,
    pub(super) account: String,
}
impl VerifiedWrap {
    pub fn entry(&self) -> &WrapEntry {
        &self.entry
    }
    pub fn account(&self) -> &str {
        &self.account
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SyncRecordFields {
    pub account: String,
    pub base_seq: u64,
    pub digest: String,
    pub epoch: u64,
    pub link: String,
    pub keyring_link: Option<String>,
}

// The transport boundary is envelope::parse_entry, not unchecked serde deserialization.
#[derive(Debug, Clone)]
pub struct KeyringEntry {
    pub seq: u64,
    pub fields: SyncRecordFields,
    pub author: SyncAuthor,
    pub body: KeyringBody,
    pub record_sig: String,
    pub wraps: Vec<WrapEntry>,
}
#[derive(Debug, Clone)]
pub struct BatchEntry {
    pub seq: u64,
    pub fields: SyncRecordFields,
    pub author: SyncAuthor,
    pub record_sig: String,
}
#[derive(Debug, Clone)]
pub enum SyncEntry {
    Keyring(KeyringEntry),
    Batch(BatchEntry),
}
impl SyncEntry {
    pub fn seq(&self) -> u64 {
        match self {
            Self::Keyring(e) => e.seq,
            Self::Batch(e) => e.seq,
        }
    }
    pub fn fields(&self) -> &SyncRecordFields {
        match self {
            Self::Keyring(e) => &e.fields,
            Self::Batch(e) => &e.fields,
        }
    }
    pub fn author(&self) -> &SyncAuthor {
        match self {
            Self::Keyring(e) => &e.author,
            Self::Batch(e) => &e.author,
        }
    }
    pub fn signature(&self) -> &str {
        match self {
            Self::Keyring(e) => &e.record_sig,
            Self::Batch(e) => &e.record_sig,
        }
    }
    pub fn body(&self) -> Option<&Value> {
        match self {
            Self::Keyring(e) => Some(&e.body),
            Self::Batch(_) => None,
        }
    }
    pub fn wire(&self) -> Value {
        let f = self.fields();
        let mut row = serde_json::json!({"seq":self.seq(),"base_seq":f.base_seq,"account":f.account,
            "epoch":f.epoch,"digest":f.digest,"link":f.link,"keyring_link":f.keyring_link,
            "author":self.author(),"record_sig":self.signature()});
        match self {
            Self::Keyring(e) => {
                row["kind"] = "keyring".into();
                row["body"] = e.body.clone();
                row["wraps"] = serde_json::json!(e.wraps);
            }
            Self::Batch(_) => row["kind"] = "batch".into(),
        }
        row
    }
}
#[derive(Debug, Clone)]
pub struct VerifiedKeyringEntry {
    pub entry: KeyringEntry,
    pub wraps: Vec<VerifiedWrap>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Head {
    pub seq: u64,
    pub link: String,
    pub keyring_link: String,
}
