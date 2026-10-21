use super::{errors::contract, wire::*};
use crate::{
    delta::{self, Decoder, Encoder},
    types::*,
};
use std::collections::HashMap;
type StateKey = (Option<(JsString, ServiceSequence)>, JsString);
fn key(address: &Option<ServiceInstanceAddress>, member: &JsString) -> StateKey {
    (
        address.as_ref().map(|a| (a.key.clone(), a.generation)),
        member.clone(),
    )
}
pub struct ServiceStateEncoder {
    codecs: HashMap<StateKey, Encoder>,
}
pub fn create_service_state_encoder() -> ServiceStateEncoder {
    ServiceStateEncoder {
        codecs: HashMap::new(),
    }
}
impl ServiceStateEncoder {
    pub fn encode_snapshot(
        &mut self,
        snapshot: ServiceSubscriptionSnapshot,
    ) -> Result<WireServiceSubscriptionSnapshot, ServiceError> {
        self.codecs.clear();
        Ok(WireServiceSubscriptionSnapshot {
            service_id: snapshot.service_id,
            mode: snapshot.mode,
            instances: snapshot
                .instances
                .into_iter()
                .map(|i| self.instance(i))
                .collect::<Result<_, _>>()?,
        })
    }
    fn instance(
        &mut self,
        snapshot: ServiceInstanceSnapshot,
    ) -> Result<WireServiceInstanceSnapshot, ServiceError> {
        let mut members = Vec::new();
        for member in snapshot.members {
            members.push(match member {
                ServiceMemberSnapshot::Method { name } => {
                    WireServiceMemberSnapshot::Method { name }
                }
                ServiceMemberSnapshot::State {
                    name,
                    sequence,
                    ops,
                } => {
                    let key = key(&snapshot.instance, &name);
                    if self.codecs.contains_key(&key) {
                        return Err(contract("Duplicate service state"));
                    }
                    let mut codec = delta::encoder();
                    let ops = codec.encode(&ops);
                    self.codecs.insert(key, codec);
                    WireServiceMemberSnapshot::State {
                        name,
                        sequence,
                        ops,
                    }
                }
            });
        }
        Ok(WireServiceInstanceSnapshot {
            instance: snapshot.instance,
            members,
        })
    }
    pub fn encode_update(
        &mut self,
        update: ServiceProviderUpdate,
    ) -> Result<WireServiceProviderUpdate, ServiceError> {
        Ok(match update {
            ServiceProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let codec = self
                    .codecs
                    .get_mut(&key(&instance, &member))
                    .ok_or_else(|| contract("Unknown service state"))?;
                WireServiceProviderUpdate::State {
                    instance,
                    member,
                    sequence,
                    ops: codec.encode(&ops),
                }
            }
            ServiceProviderUpdate::Reset { snapshot } => WireServiceProviderUpdate::Reset {
                snapshot: self.encode_snapshot(snapshot)?,
            },
            ServiceProviderUpdate::Replaced { snapshot } => {
                self.codecs.clear();
                WireServiceProviderUpdate::Replaced {
                    snapshot: self.instance(snapshot)?,
                }
            }
            ServiceProviderUpdate::Spawned { instance } => WireServiceProviderUpdate::Spawned {
                instance: self.instance(instance)?,
            },
            ServiceProviderUpdate::Unavailable => {
                self.codecs.clear();
                WireServiceProviderUpdate::Unavailable
            }
            ServiceProviderUpdate::Closed { instance } => {
                self.codecs.retain(|(address, _), _| {
                    address
                        .as_ref()
                        .is_none_or(|(k, g)| k != &instance.key || *g != instance.generation)
                });
                WireServiceProviderUpdate::Closed { instance }
            }
        })
    }
}
pub struct ServiceStateDecoder {
    codecs: HashMap<StateKey, Decoder>,
}
pub fn create_service_state_decoder() -> ServiceStateDecoder {
    ServiceStateDecoder {
        codecs: HashMap::new(),
    }
}
impl ServiceStateDecoder {
    pub fn decode_snapshot(
        &mut self,
        snapshot: WireServiceSubscriptionSnapshot,
    ) -> Result<ServiceSubscriptionSnapshot, ServiceError> {
        self.codecs.clear();
        Ok(ServiceSubscriptionSnapshot {
            service_id: snapshot.service_id,
            mode: snapshot.mode,
            instances: snapshot
                .instances
                .into_iter()
                .map(|i| self.instance(i))
                .collect::<Result<_, _>>()?,
        })
    }
    fn instance(
        &mut self,
        snapshot: WireServiceInstanceSnapshot,
    ) -> Result<ServiceInstanceSnapshot, ServiceError> {
        let mut members = Vec::new();
        for member in snapshot.members {
            members.push(match member {
                WireServiceMemberSnapshot::Method { name } => {
                    ServiceMemberSnapshot::Method { name }
                }
                WireServiceMemberSnapshot::State {
                    name,
                    sequence,
                    ops,
                } => {
                    let key = key(&snapshot.instance, &name);
                    if self.codecs.contains_key(&key) {
                        return Err(contract("Duplicate service state"));
                    }
                    let mut codec = delta::decoder();
                    let ops = codec.decode(&ops)?;
                    self.codecs.insert(key, codec);
                    ServiceMemberSnapshot::State {
                        name,
                        sequence,
                        ops,
                    }
                }
            });
        }
        Ok(ServiceInstanceSnapshot {
            instance: snapshot.instance,
            members,
        })
    }
    pub fn decode_update(
        &mut self,
        update: WireServiceProviderUpdate,
    ) -> Result<ServiceProviderUpdate, ServiceError> {
        Ok(match update {
            WireServiceProviderUpdate::State {
                instance,
                member,
                sequence,
                ops,
            } => {
                let codec = self
                    .codecs
                    .get_mut(&key(&instance, &member))
                    .ok_or_else(|| contract("Unknown service state"))?;
                ServiceProviderUpdate::State {
                    instance,
                    member,
                    sequence,
                    ops: codec.decode(&ops)?,
                }
            }
            WireServiceProviderUpdate::Reset { snapshot } => ServiceProviderUpdate::Reset {
                snapshot: self.decode_snapshot(snapshot)?,
            },
            WireServiceProviderUpdate::Replaced { snapshot } => {
                self.codecs.clear();
                ServiceProviderUpdate::Replaced {
                    snapshot: self.instance(snapshot)?,
                }
            }
            WireServiceProviderUpdate::Spawned { instance } => ServiceProviderUpdate::Spawned {
                instance: self.instance(instance)?,
            },
            WireServiceProviderUpdate::Unavailable => {
                self.codecs.clear();
                ServiceProviderUpdate::Unavailable
            }
            WireServiceProviderUpdate::Closed { instance } => {
                self.codecs.retain(|(address, _), _| {
                    address
                        .as_ref()
                        .is_none_or(|(k, g)| k != &instance.key || *g != instance.generation)
                });
                ServiceProviderUpdate::Closed { instance }
            }
        })
    }
}
