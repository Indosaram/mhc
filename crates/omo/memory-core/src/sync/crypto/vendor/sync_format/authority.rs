//! Running authority and indexes for the incremental keyring fold.
use super::fold::AuthorityFold;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
#[derive(Debug, Clone, Default)]
pub struct FoldState {
    pub authority: AuthorityFold,
    pub(super) done: Vec<Value>,
    pub(super) by_digest: BTreeMap<String, usize>,
    pub(super) by_seq: BTreeMap<u64, usize>,
    pub(super) before_join: BTreeMap<u64, AuthorityFold>,
    pub(super) vetoed: BTreeSet<String>,
    pub(super) last_seq: u64,
}
