//! Port of senpi `packages/agent/src/execution/remote-epoch.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::json;

use crate::durable::store::{DurableStore, StoreValue, StoreWrite};
use crate::durable::store_admission::AdmissionLine;

use super::remote_protocol::{EpochRefusal, RemoteHandMessage};

pub const EPOCH_FLOOR_MAX_STEP: u64 = 1;
pub const EPOCH_FLOOR_PREFIX: &str = "hand.epoch-floor:";
pub const HAND_SELF_SENDER: &str = "self";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorRaise {
    Acked { floor: u64 },
    Refused { reason: EpochRefusal },
}

pub struct DurableEpochOptions {
    pub store: Arc<dyn DurableStore>,
    pub epoch: u64,
    pub admission: AdmissionLine,
}

pub type OwnerOfFn = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

pub struct HandEpochFenceOptions {
    pub owner_of: OwnerOfFn,
    pub durable: Option<DurableEpochOptions>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Floor {
    epoch: u64,
    owner: String,
}

fn decode_floor(data: &StoreValue) -> Result<Floor, String> {
    let Some(obj) = data.as_object() else {
        return Err("A stored epoch floor is malformed".to_owned());
    };
    let Some(epoch) = obj
        .get("epoch")
        .and_then(StoreValue::as_u64)
        .filter(|&e| e >= 1)
    else {
        return Err("A stored epoch floor is malformed".to_owned());
    };
    let Some(owner) = obj.get("owner").and_then(StoreValue::as_str) else {
        return Err("A stored epoch floor is malformed".to_owned());
    };
    Ok(Floor {
        epoch,
        owner: owner.to_owned(),
    })
}

pub struct HandEpochFence {
    owner_of: OwnerOfFn,
    durable: Option<DurableEpochOptions>,
    floors: Arc<Mutex<HashMap<String, Floor>>>,
    init_once: tokio::sync::OnceCell<Result<(), String>>,
}

impl HandEpochFence {
    pub async fn ready(&self) -> Result<(), String> {
        self.init_once
            .get_or_init(|| async {
                let Some(durable) = &self.durable else {
                    return Ok(());
                };
                let docs = durable
                    .store
                    .scan_documents(EPOCH_FLOOR_PREFIX)
                    .await
                    .map_err(|e| e.to_string())?;
                let mut map = self.floors.lock().unwrap_or_else(|p| p.into_inner());
                for doc in docs {
                    if let Some(scope) = doc.key.strip_prefix(EPOCH_FLOOR_PREFIX) {
                        map.insert(scope.to_owned(), decode_floor(&doc.data)?);
                    }
                }
                Ok(())
            })
            .await
            .clone()
    }

    async fn raise_now(&self, scope: &str, epoch: u64, sender: &str) -> Result<FloorRaise, String> {
        if (self.owner_of)(scope).as_deref() != Some(sender) {
            return Ok(FloorRaise::Refused {
                reason: EpochRefusal::PeerUnauthorized,
            });
        }
        let current = self
            .floors
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(scope)
            .cloned();
        let floor = current.as_ref().map_or(0, |f| f.epoch);
        if epoch < floor {
            return Ok(FloorRaise::Acked { floor });
        }
        if let Some(curr) = &current
            && epoch == floor
        {
            return Ok(if curr.owner == sender {
                FloorRaise::Acked { floor }
            } else {
                FloorRaise::Refused {
                    reason: EpochRefusal::EpochFenced,
                }
            });
        }
        if epoch - floor > EPOCH_FLOOR_MAX_STEP {
            return Ok(FloorRaise::Refused {
                reason: EpochRefusal::EpochStepExceeded,
            });
        }
        let next = Floor {
            epoch,
            owner: sender.to_owned(),
        };
        if let Some(durable) = &self.durable {
            let write = StoreWrite::Put {
                key: format!("{EPOCH_FLOOR_PREFIX}{scope}"),
                data: json!({ "epoch": next.epoch, "owner": next.owner }),
            };
            let floors = Arc::clone(&self.floors);
            let scope_owned = scope.to_owned();
            let next_adopt = next.clone();
            durable
                .store
                .commit(
                    durable.epoch,
                    vec![write],
                    Box::new(move |_| {
                        floors
                            .lock()
                            .unwrap_or_else(|p| p.into_inner())
                            .insert(scope_owned, next_adopt);
                    }),
                )
                .await
                .map_err(|e| e.to_string())?;
        } else {
            self.floors
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .insert(scope.to_owned(), next);
        }
        Ok(FloorRaise::Acked { floor: epoch })
    }

    pub async fn raise(&self, scope: &str, epoch: u64, sender: &str) -> Result<FloorRaise, String> {
        self.ready().await?;
        let scope_owned = scope.to_owned();
        let sender_owned = sender.to_owned();
        if let Some(durable) = &self.durable {
            durable
                .admission
                .admit(|| async move { self.raise_now(&scope_owned, epoch, &sender_owned).await })
                .await
        } else {
            self.raise_now(&scope_owned, epoch, &sender_owned).await
        }
    }

    pub fn admits(&self, scope: &str, epoch: Option<u64>, sender: &str) -> bool {
        let floors = self.floors.lock().unwrap_or_else(|p| p.into_inner());
        let Some(floor) = floors.get(scope) else {
            return true;
        };
        let at = epoch.unwrap_or(0);
        at > floor.epoch || (at == floor.epoch && floor.owner == sender)
    }
}

pub fn create_hand_epoch_fence(options: HandEpochFenceOptions) -> Arc<HandEpochFence> {
    Arc::new(HandEpochFence {
        owner_of: options.owner_of,
        durable: options.durable,
        floors: Arc::new(Mutex::new(HashMap::new())),
        init_once: tokio::sync::OnceCell::new(),
    })
}

pub async fn answer_floor(
    fence: Option<&Arc<HandEpochFence>>,
    scope: &str,
    epoch: u64,
    sender: &str,
) -> Result<RemoteHandMessage, String> {
    let Some(fence) = fence else {
        return Ok(RemoteHandMessage::FloorRefused {
            reason: EpochRefusal::EpochUnsupported,
        });
    };
    match fence.raise(scope, epoch, sender).await? {
        FloorRaise::Acked { floor } => Ok(RemoteHandMessage::FloorAck { floor }),
        FloorRaise::Refused { reason } => Ok(RemoteHandMessage::FloorRefused { reason }),
    }
}
