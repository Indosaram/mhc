use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::lane::{Lane, encoded};
use super::types::LaneState;
use crate::harness::context::Context;
use crate::harness::events::{HarnessEvent, HarnessEventBus, HarnessEventPayload};
use crate::harness::session::session::{SessionError, SessionErrorKind, session_invariant_error};
use crate::harness::session::types::{LaneConfiguration, Operation, Session, Write};
use crate::harness::session::values::*;

pub struct Harness {
    pub session: Arc<dyn Session>,
    pub events: HarnessEventBus,
    pub lanes_by_name: Mutex<BTreeMap<String, Arc<Lane>>>,
    seed: LaneConfiguration,
    closed_error: Mutex<Option<SessionError>>,
    close_lock: tokio::sync::Mutex<()>,
}

impl Harness {
    fn assert_open(&self) -> Result<(), SessionError> {
        match self
            .closed_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub async fn lane(
        &self,
        name: &str,
        create_at: Option<String>,
        context: &Context,
    ) -> Result<Arc<Lane>, SessionError> {
        self.assert_open()?;
        if name.is_empty() || name.contains('\0') {
            let reason = if name.is_empty() {
                "lane name must not be empty"
            } else {
                "lane name must not contain \\u0000"
            };
            return Err(SessionError::new(
                SessionErrorKind::InvalidBranch,
                format!("Invalid lane {}: {reason}", serde_json::json!(name)),
            ));
        }
        let mutation = self.session.begin_mutation(context).await?;
        let result = async {
            self.assert_open()?;
            if let Some(lane) = self
                .lanes_by_name
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(name)
                .cloned()
            {
                return Ok((lane, None));
            }
            let tip = mutation.get_value(&branch_tip(name), context).await?;
            let config = mutation.get_value(&lane_config(name), context).await?;
            let durable = mutation.get_value(&lane_state(name), context).await?;
            if config.is_some() != durable.is_some() || (config.is_some() && tip.is_none()) {
                return Err(session_invariant_error(format!(
                    "Lane {} has partial durable state",
                    serde_json::json!(name)
                )));
            }
            let exists = config.is_some();
            let tip_id = match &tip {
                Some(stored) => serde_json::from_value(stored.value.clone())
                    .map_err(|e| session_invariant_error(e.to_string()))?,
                None => create_at,
            };
            if tip.is_none()
                && let Some(id) = &tip_id
                && !mutation
                    .get_entries(vec![id.clone()], context)
                    .await?
                    .contains_key(id)
            {
                return Err(SessionError::new(
                    SessionErrorKind::UnknownTarget,
                    format!("Unknown target: {id}"),
                ));
            }
            let configuration = match config {
                Some(stored) => serde_json::from_value(stored.value)
                    .map_err(|e| session_invariant_error(e.to_string()))?,
                None => self.seed.clone(),
            };
            let stored: crate::harness::session::types::LaneState = match durable {
                Some(stored) => serde_json::from_value(stored.value)
                    .map_err(|e| session_invariant_error(e.to_string()))?,
                None => crate::harness::session::types::LaneState {
                    current_operation_id: None,
                    last_operation_id: None,
                    inbox: vec![],
                },
            };
            let operation = match &stored.current_operation_id {
                Some(id) => {
                    let meta = mutation
                        .get_value(&operation_meta(id), context)
                        .await?
                        .ok_or_else(|| session_invariant_error("Missing operation metadata"))?;
                    let state = mutation
                        .get_value(&operation_state(id), context)
                        .await?
                        .ok_or_else(|| session_invariant_error("Missing operation state"))?;
                    Some(Operation {
                        meta: serde_json::from_value(meta.value)
                            .map_err(|e| session_invariant_error(e.to_string()))?,
                        state: serde_json::from_value(state.value)
                            .map_err(|e| session_invariant_error(e.to_string()))?,
                    })
                }
                None => None,
            };
            if !exists {
                let mut writes = vec![];
                if tip.is_none() {
                    writes.push(Write::Value(set_value(
                        &branch_tip(name),
                        encoded(&tip_id)?,
                    )));
                }
                writes.push(Write::Value(set_value(
                    &lane_config(name),
                    encoded(&configuration)?,
                )));
                writes.push(Write::Value(set_value(
                    &lane_state(name),
                    encoded(&stored)?,
                )));
                mutation.commit(writes, context).await?;
            }
            let lane = Arc::new(Lane::new(
                name.into(),
                self.session.clone(),
                LaneState {
                    tip_id: tip_id.clone(),
                    configuration,
                    inbox: stored.inbox,
                    last_operation_id: stored.last_operation_id,
                    operation,
                },
                self.events.clone(),
            ));
            self.lanes_by_name
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(name.into(), lane.clone());
            let delivery = (!exists).then(|| {
                self.events.begin_emit_batch(
                    vec![HarnessEvent::new(
                        HarnessEventPayload::LaneCreated { at: tip_id },
                        Some(name.into()),
                    )],
                    context.clone(),
                )
            });
            Ok((lane, delivery))
        }
        .await;
        mutation.end(context).await;
        let (lane, delivery) = result?;
        if let Some(delivery) = delivery {
            delivery.await;
        }
        Ok(lane)
    }

    pub fn lanes(&self) -> Result<Vec<(String, LaneState)>, SessionError> {
        self.assert_open()?;
        Ok(self
            .lanes_by_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(name, lane)| (name.clone(), lane.state()))
            .collect())
    }

    pub async fn get_name(&self, context: &Context) -> Result<Option<String>, SessionError> {
        self.assert_open()?;
        self.session.get_name(context).await
    }
    pub async fn get_label(
        &self,
        id: &str,
        context: &Context,
    ) -> Result<Option<String>, SessionError> {
        self.assert_open()?;
        self.session.get_label(id, context).await
    }

    pub async fn set_name(
        &self,
        name: Option<String>,
        context: &Context,
    ) -> Result<(), SessionError> {
        self.assert_open()?;
        let mutation = self.session.begin_mutation(context).await?;
        let result = async {
            self.assert_open()?;
            let write = match &name {
                Some(name) => set_value(&session_name(), encoded(name)?),
                None => delete_value(&session_name()),
            };
            mutation.commit(vec![Write::Value(write)], context).await?;
            Ok(self.events.begin_emit_batch(
                vec![HarnessEvent::new(
                    HarnessEventPayload::ValueUpdate {
                        value: "session_name".into(),
                        name,
                        target_id: None,
                        label: None,
                    },
                    None,
                )],
                context.clone(),
            ))
        }
        .await;
        mutation.end(context).await;
        result?.await;
        Ok(())
    }

    pub async fn set_label(
        &self,
        id: &str,
        label: Option<String>,
        context: &Context,
    ) -> Result<(), SessionError> {
        self.assert_open()?;
        let mutation = self.session.begin_mutation(context).await?;
        let result = async {
            self.assert_open()?;
            let write = match &label {
                Some(label) => set_value(&entry_label(id), encoded(label)?),
                None => delete_value(&entry_label(id)),
            };
            mutation.commit(vec![Write::Value(write)], context).await?;
            Ok(self.events.begin_emit_batch(
                vec![HarnessEvent::new(
                    HarnessEventPayload::ValueUpdate {
                        value: "entry_label".into(),
                        name: None,
                        target_id: Some(id.into()),
                        label,
                    },
                    None,
                )],
                context.clone(),
            ))
        }
        .await;
        mutation.end(context).await;
        result?.await;
        Ok(())
    }

    pub async fn close(&self, context: &Context) {
        let _guard = self.close_lock.lock().await;
        if self.assert_open().is_err() {
            return;
        }
        let error = SessionError::new(SessionErrorKind::Closed, "AgentHarness is closed");
        *self.closed_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(error.clone());
        for lane in self
            .lanes_by_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            lane.seal(error.clone());
        }
        self.events.close(error.to_string());
        self.session.close(context).await;
    }
}

pub async fn create_agent_harness(
    session: Arc<dyn Session>,
    seed: LaneConfiguration,
    context: &Context,
) -> Result<Harness, SessionError> {
    let harness = Harness {
        session,
        events: HarnessEventBus::new(),
        lanes_by_name: Mutex::new(BTreeMap::new()),
        seed,
        closed_error: Mutex::new(None),
        close_lock: tokio::sync::Mutex::new(()),
    };
    let configurations = harness
        .session
        .scan_values(
            &Value {
                namespace: "pi.lane.config".into(),
                key: String::new(),
            },
            context,
        )
        .await?;
    for configuration in configurations {
        harness
            .lane(&configuration.address.key, None, context)
            .await?;
    }
    Ok(harness)
}
