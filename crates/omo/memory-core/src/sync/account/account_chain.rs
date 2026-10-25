use super::http::AccountHttp;
use crate::sync::{
    crypto::{
        anchor::ChainAnchor,
        chain::{AcceptedBatch, AppendOutcome, ChainState, ChainStep, append_entries, empty_chain},
        rejection::RejectionCounter,
    },
    engine::error::{EngineError, EngineResult},
    keyring_port::DeviceKeyring,
    recovery::{
        keyring_append::own_material,
        served_chain::{committed_wrap, expected_history},
    },
};
use std::collections::BTreeMap;
#[derive(Clone, Debug)]
pub enum ChainPosition {
    Batch(AcceptedBatch),
    Keyring,
    Vetoed,
    Refused(&'static str),
    Pending,
    Unknown,
}
pub struct AccountChain {
    pub account: String,
    state: ChainState,
    keyring: std::rc::Rc<dyn DeviceKeyring>,
    http: AccountHttp,
    rejections: RejectionCounter,
    positions: BTreeMap<u64, ChainPosition>,
    links: BTreeMap<u64, String>,
    refused_at: Option<u64>,
    held_from: Option<u64>,
    keys: BTreeMap<u64, Vec<u8>>,
    keys_epoch: Option<u64>,
}
pub async fn open_account_chain(
    account: String,
    anchor: ChainAnchor,
    keyring: std::rc::Rc<dyn DeviceKeyring>,
    http: AccountHttp,
    rejections: RejectionCounter,
) -> EngineResult<AccountChain> {
    let mut chain = AccountChain {
        state: empty_chain(account.clone(), anchor),
        account,
        keyring,
        http,
        rejections,
        positions: BTreeMap::new(),
        links: BTreeMap::new(),
        refused_at: None,
        held_from: None,
        keys: BTreeMap::new(),
        keys_epoch: None,
    };
    chain.extend(u64::MAX).await?;
    Ok(chain)
}
impl AccountChain {
    pub fn state(&self) -> &ChainState {
        &self.state
    }
    pub fn is_member(&self) -> bool {
        self.state
            .authority()
            .is_some_and(|a| a.members.contains_key(&self.keyring.principal().id))
    }
    pub fn link_at(&self, seq: u64) -> Option<&str> {
        self.links.get(&seq).map(String::as_str)
    }
    async fn read_page(&mut self) -> EngineResult<usize> {
        let after = self.state.head().map_or(0, |h| h.seq);
        let rows = self.http.list_entries_after(after).await?.ok_or_else(|| {
            EngineError::Other("account sync: the entries read did not answer a list".into())
        })?;
        if rows.is_empty() {
            return Ok(0);
        }
        let keys = if rows.iter().any(|r| r["kind"] == "keyring") {
            self.http.list_keys().await?.ok_or_else(|| {
                EngineError::Other("account sync: the keyring read did not answer a list".into())
            })?
        } else {
            Vec::new()
        };
        let by_seq: BTreeMap<_, _> = keys
            .iter()
            .filter_map(|r| Some((r["seq"].as_u64()?, r)))
            .collect();
        let mut entries = Vec::new();
        for row in &rows {
            let served = row.is_object()
                && row["seq"]
                    .as_u64()
                    .is_some_and(|s| s <= 9_007_199_254_740_991)
                && (row["kind"] == "batch" || row["kind"] == "keyring");
            let entry = if !served {
                None
            } else if row["kind"] == "batch" {
                let mut entry = row.clone();
                entry["keyring_link"] = serde_json::Value::Null;
                Some(entry)
            } else {
                by_seq
                    .get(&row["seq"].as_u64().unwrap_or(0))
                    .filter(|full| {
                        full["digest"] == row["digest"]
                            && full["keyring_link"] == row["keyring_link"]
                    })
                    .map(|full| (**full).clone())
            };
            let Some(entry) = entry else {
                self.rejections.record(if served {
                    "keyring_mismatch"
                } else {
                    "entry_unreadable"
                });
                self.held_from = Some(after + entries.len() as u64 + 1);
                break;
            };
            entries.push(entry);
        }
        if !entries.is_empty() {
            let outcome = append_entries(&self.state, &entries, &mut self.rejections);
            let (state, steps, refused) = match outcome {
                AppendOutcome::Accepted { state, steps } => (state, steps, None),
                AppendOutcome::Rejected {
                    state,
                    steps,
                    seq,
                    reason,
                } => (state, steps, Some((seq, reason))),
            };
            for step in steps {
                match step {
                    ChainStep::Batch(batch) => {
                        self.positions
                            .insert(batch.seq, ChainPosition::Batch(batch));
                    }
                    ChainStep::Keyring { seq, .. } => {
                        self.positions.insert(seq, ChainPosition::Keyring);
                    }
                    ChainStep::Vetoed { seq } => {
                        self.positions.insert(seq, ChainPosition::Vetoed);
                    }
                }
            }
            for entry in &entries {
                if let (Some(seq), Some(link)) = (entry["seq"].as_u64(), entry["link"].as_str())
                    && seq <= state.head().map_or(0, |h| h.seq) {
                        self.links.insert(seq, link.into());
                    }
            }
            self.state = state;
            if let Some((seq, reason)) = refused {
                self.positions.insert(seq, ChainPosition::Refused(reason));
                self.refused_at = Some(seq);
            }
        }
        Ok(rows.len())
    }
    async fn extend(&mut self, target: u64) -> EngineResult<()> {
        self.held_from = None;
        loop {
            let before = self.state.head().map_or(0, |h| h.seq);
            let served = self.read_page().await?;
            let head = self.state.head().map_or(0, |h| h.seq);
            if served == 0
                || self.refused_at.is_some()
                || self.held_from.is_some()
                || head >= target
                || head == before
            {
                return Ok(());
            }
        }
    }
    pub async fn reach(&mut self, seq: u64) -> EngineResult<ChainPosition> {
        if self.refused_at.is_none() && self.state.head().map_or(0, |h| h.seq) < seq {
            self.extend(seq).await?;
        }
        if let Some(position) = self.positions.get(&seq) {
            return Ok(position.clone());
        }
        if let Some(at) = self.refused_at
            && seq > at {
                return Ok(self
                    .positions
                    .get(&at)
                    .cloned()
                    .unwrap_or(ChainPosition::Unknown));
            }
        if self.held_from.is_some_and(|at| seq >= at) {
            return Ok(ChainPosition::Pending);
        }
        Ok(ChainPosition::Unknown)
    }
    pub fn epoch_key(&mut self, epoch: u64) -> Option<Vec<u8>> {
        let authority = self.state.authority()?;
        if self.keys_epoch != Some(authority.epoch) {
            let wrap = committed_wrap(
                &self.state,
                authority.epoch,
                &format!("device:{}", self.keyring.principal().id),
            )?;
            let material = own_material(
                self.keyring.as_ref(),
                &self.account,
                &wrap,
                &expected_history(authority, authority.epoch),
            )?;
            self.keys.clear();
            self.keys.insert(material.epoch, material.current.to_vec());
            for older in material.history {
                self.keys.insert(older.epoch, older.key.to_vec());
            }
            self.keys_epoch = Some(authority.epoch);
        }
        self.keys.get(&epoch).cloned()
    }
}
