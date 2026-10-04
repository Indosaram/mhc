use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::lane::{Lane, encoded};
use super::types::{Config, LaneState};
use crate::harness::agent_harness::AgentHarnessOptions;
use crate::harness::context::Context;
use crate::harness::events::{HarnessEvent, HarnessEventBus, HarnessEventPayload};
use crate::harness::hooks::{HookErrorReporter, HookRegistry};
use crate::harness::session::session::{SessionError, SessionErrorKind, session_invariant_error};
use crate::harness::session::types::{Control, LaneConfiguration, LaneModelRef, Operation, OperationIntentKind, Session, Write};
use crate::harness::session::values::*;

/// Pinned `DEFAULT_RETRY_POLICY`/`DEFAULT_COMPACTION_SETTINGS`-backed process-local drive config.
pub fn default_drive_config() -> Config<()> {
    Config {
        tools: Vec::new(),
        resources: Default::default(),
        stream_options: Default::default(),
        retry_policy: crate::harness::config::default_retry_policy(),
        compaction: crate::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS,
        steering_mode: crate::types::QueueMode::All,
        follow_up_mode: crate::types::QueueMode::All,
        tool_execution: crate::harness::session::types::ToolExecutionMode::Parallel,
        tool_context: None,
        system_prompt: None,
        to_provider_messages: Arc::new(|messages, _context| {
            Box::pin(async move { crate::harness::messages::convert_to_llm(messages) })
        }),
        entry_projectors: Default::default(),
    }
}

fn noop_hook_reporter() -> HookErrorReporter {
    Arc::new(|_, _, _, _| Box::pin(async {}))
}

pub struct Harness {
    pub session: Arc<dyn Session>,
    pub events: HarnessEventBus,
    pub models: maho_ai::models::Models,
    pub hooks: Arc<HookRegistry>,
    pub lanes_by_name: Mutex<BTreeMap<String, Arc<Lane>>>,
    seed: LaneConfiguration,
    closed_error: Mutex<Option<SessionError>>,
    close_lock: tokio::sync::Mutex<()>,
    config: Arc<Mutex<Config<()>>>,
    session_closed: Mutex<bool>,
}

/// One lane operation left open by a previous worker, mirroring pinned `OpenOperation`
/// (`agent-harness.ts:211-217`). `aborting` is set when the durable control is `cancel_requested`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenOperation {
    pub lane: String,
    pub operation_id: String,
    pub kind: OperationIntentKind,
    pub started_at: i64,
    pub aborting: bool,
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
            let mut lane = Lane::new(
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
            );
            lane.drive_config = self.config.clone();
            lane.models = self.models.clone();
            lane.hooks = self.hooks.clone();
            let lane = Arc::new(lane);
            lane.install_self();
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

    /// Every loaded lane's active operation, matching pinned `createAgentHarness`'s `open` list so a
    /// worker can re-drive recoveries after its services are reachable.
    pub fn open_operations(&self) -> Result<Vec<OpenOperation>, SessionError> {
        self.assert_open()?;
        let lanes = self.lanes_by_name.lock().unwrap_or_else(|error| error.into_inner());
        let mut open = Vec::new();
        for (name, lane) in lanes.iter() {
            let state = lane.state();
            let Some(operation) = state.operation else { continue; };
            let aborting = matches!(operation.state.operation_scope_of().control, Control::CancelRequested { .. });
            open.push(OpenOperation {
                lane: name.clone(),
                operation_id: operation.meta.operation_id,
                kind: operation.meta.intent.kind(),
                started_at: operation.meta.started_at,
                aborting,
            });
        }
        Ok(open)
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
        if *self.session_closed.lock().unwrap_or_else(|error| error.into_inner()) {
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
        self.hooks.close(error.to_string());
        self.events.close(error.to_string());
        let lanes: Vec<_> = self.lanes_by_name.lock().unwrap_or_else(|error| error.into_inner()).values().cloned().collect();
        for lane in lanes { lane.finish_idle_callback().await; }
        self.session.close(context).await;
        *self.session_closed.lock().unwrap_or_else(|error| error.into_inner()) = true;
    }

    pub fn fault(&self, cause: SessionError, context: &Context) -> SessionError {
        let error = {
            let mut closed = self.closed_error.lock().unwrap_or_else(|error| error.into_inner());
            if let Some(error) = &*closed { return error.clone(); }
            let error = session_invariant_error(format!("AgentHarness storage or invariant fault: {cause}"));
            *closed = Some(error.clone());
            error
        };
        for lane in self.lanes_by_name.lock().unwrap_or_else(|error| error.into_inner()).values() { lane.seal(error.clone()); }
        let delivery = self.events.begin_emit_batch(vec![HarnessEvent::new(HarnessEventPayload::Fault { code: "harness_fault".into(), message: "AgentHarness storage or invariant fault".into() }, None)], context.clone());
        drop(delivery);
        self.hooks.close(error.to_string());
        self.events.close(error.to_string());
        error
    }

    pub fn get_resources(&self) -> Result<crate::harness::types::AgentHarnessResources, SessionError> { self.assert_open()?; Ok(self.config.lock().unwrap_or_else(|error| error.into_inner()).resources.clone()) }
    pub async fn set_resources(&self, resources: crate::harness::types::AgentHarnessResources, context: &Context) -> Result<(), SessionError> {
        self.set_config(move |config| {
            let as_value = |value: &crate::harness::types::AgentHarnessResources| -> Result<serde_json::Value, SessionError> {
                let mut object = serde_json::Map::new();
                if let Some(skills) = &value.skills { object.insert("skills".into(), encoded(skills)?); }
                if let Some(templates) = &value.prompt_templates { object.insert("promptTemplates".into(), encoded(templates)?); }
                Ok(serde_json::Value::Object(object))
            };
            let previous = as_value(&config.resources)?;
            let value = as_value(&resources)?;
            config.resources = resources;
            Ok(HarnessEventPayload::ConfigUpdate { property: "resources".into(), previous, value })
        }, context).await
    }
    pub fn get_stream_options(&self) -> Result<crate::harness::types::AgentHarnessStreamOptions, SessionError> { self.assert_open()?; Ok(self.config.lock().unwrap_or_else(|error| error.into_inner()).stream_options.clone()) }
    pub fn get_retry_policy(&self) -> Result<maho_ai::utils::retry::RetryPolicy, SessionError> { self.assert_open()?; Ok(self.config.lock().unwrap_or_else(|error| error.into_inner()).retry_policy.clone()) }
    pub fn get_compaction_settings(&self) -> Result<crate::harness::compaction::compaction::CompactionSettings, SessionError> { self.assert_open()?; Ok(self.config.lock().unwrap_or_else(|error| error.into_inner()).compaction) }
    pub fn get_steering_mode(&self) -> Result<crate::types::QueueMode, SessionError> { self.assert_open()?; Ok(self.config.lock().unwrap_or_else(|error| error.into_inner()).steering_mode) }
    pub fn get_follow_up_mode(&self) -> Result<crate::types::QueueMode, SessionError> { self.assert_open()?; Ok(self.config.lock().unwrap_or_else(|error| error.into_inner()).follow_up_mode) }

    async fn set_config<F>(&self, update: F, context: &Context) -> Result<(), SessionError>
    where F: FnOnce(&mut Config<()>) -> Result<HarnessEventPayload, SessionError> {
        self.assert_open()?;
        let event = { let mut config = self.config.lock().unwrap_or_else(|error| error.into_inner()); update(&mut config)? };
        self.events.emit(HarnessEvent::new(event, None), context.clone()).await;
        Ok(())
    }

    pub async fn set_stream_options(&self, value: crate::harness::types::AgentHarnessStreamOptions, context: &Context) -> Result<(), SessionError> {
        self.set_config(move |config| { let previous = encoded(&config.stream_options)?; let event_value = encoded(&value)?; config.stream_options = value; Ok(HarnessEventPayload::ConfigUpdate { property: "streamOptions".into(), previous, value: event_value }) }, context).await
    }
    pub async fn set_retry_policy(&self, value: maho_ai::utils::retry::RetryPolicy, context: &Context) -> Result<(), SessionError> {
        crate::harness::config::validate_retry_policy(&value).map_err(session_invariant_error)?;
        self.set_config(move |config| { let previous = retry_policy_value(&config.retry_policy); let event_value = retry_policy_value(&value); config.retry_policy = value; Ok(HarnessEventPayload::ConfigUpdate { property: "retryPolicy".into(), previous, value: event_value }) }, context).await
    }
    pub async fn set_compaction_settings(&self, value: crate::harness::compaction::compaction::CompactionSettings, context: &Context) -> Result<(), SessionError> {
        crate::harness::config::validate_compaction_settings(&value).map_err(session_invariant_error)?;
        self.set_config(move |config| { let previous = encoded(&config.compaction)?; let event_value = encoded(&value)?; config.compaction = value; Ok(HarnessEventPayload::ConfigUpdate { property: "compactionSettings".into(), previous, value: event_value }) }, context).await
    }
    pub async fn set_steering_mode(&self, value: crate::types::QueueMode, context: &Context) -> Result<(), SessionError> {
        self.set_config(move |config| { let previous = encoded(&config.steering_mode)?; config.steering_mode = value; Ok(HarnessEventPayload::ConfigUpdate { property: "steeringMode".into(), previous, value: encoded(&value)? }) }, context).await
    }
    pub async fn set_follow_up_mode(&self, value: crate::types::QueueMode, context: &Context) -> Result<(), SessionError> {
        self.set_config(move |config| { let previous = encoded(&config.follow_up_mode)?; config.follow_up_mode = value; Ok(HarnessEventPayload::ConfigUpdate { property: "followUpMode".into(), previous, value: encoded(&value)? }) }, context).await
    }

    /// Pinned `Harness.buildLane`: attach the session, models, hooks and shared config to a lane.
    fn build_lane(&self, name: &str, state: LaneState) -> Arc<Lane> {
        let mut lane = Lane::new(name.to_owned(), self.session.clone(), state, self.events.clone());
        lane.drive_config = self.config.clone();
        lane.models = self.models.clone();
        lane.hooks = self.hooks.clone();
        let lane = Arc::new(lane);
        lane.install_self();
        lane
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
        models: maho_ai::models::create_models(None),
        hooks: Arc::new(HookRegistry::new(noop_hook_reporter())),
        lanes_by_name: Mutex::new(BTreeMap::new()),
        seed,
        closed_error: Mutex::new(None),
        close_lock: tokio::sync::Mutex::new(()),
        config: Arc::new(Mutex::new(default_drive_config())),
        session_closed: Mutex::new(false),
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

fn retry_policy_value(policy: &maho_ai::utils::retry::RetryPolicy) -> serde_json::Value {
    let mut value = serde_json::json!({ "enabled": policy.enabled, "maxRetries": policy.max_retries, "baseDelayMs": policy.base_delay_ms });
    if let Some(delay) = policy.max_agent_delay_ms { value["maxAgentDelayMs"] = serde_json::json!(delay); }
    value
}

/// Pinned `createAgentHarness(options, context)`: attach the durable harness to one open session,
/// installing the full process-local `Config` (tools, resources, stream options, retry, compaction,
/// queue modes, tool execution, system prompt) and restoring every durable lane.
pub async fn create_agent_harness_with_options(
    options: AgentHarnessOptions,
    context: &Context,
) -> Result<(Harness, Vec<OpenOperation>), SessionError> {
    crate::harness::config::validate_tool_names(&options.tools.iter().map(|tool| tool.name().to_owned()).collect::<Vec<_>>())
        .map_err(session_invariant_error)?;
    crate::harness::config::validate_retry_policy(&options.retry.clone().unwrap_or_else(crate::harness::config::default_retry_policy))
        .map_err(session_invariant_error)?;
    crate::harness::config::validate_compaction_settings(&options.compaction.unwrap_or(crate::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS))
        .map_err(session_invariant_error)?;
    let seed = LaneConfiguration {
        model: LaneModelRef { provider: options.model.provider.clone(), model_id: options.model.id.clone() },
        thinking_level: options.thinking_level.unwrap_or(maho_ai::types::ModelThinkingLevel::Off),
        active_tool_names: options
            .active_tool_names
            .clone()
            .unwrap_or_else(|| options.tools.iter().map(|tool| tool.name().to_owned()).collect()),
    };
    let config = Config {
        tools: options.tools.clone(),
        resources: options.resources.clone().unwrap_or_default(),
        stream_options: options.stream_options.clone().unwrap_or_default(),
        retry_policy: options.retry.clone().unwrap_or_else(crate::harness::config::default_retry_policy),
        compaction: options.compaction.unwrap_or(crate::harness::compaction::compaction::DEFAULT_COMPACTION_SETTINGS),
        steering_mode: options.steering_mode.unwrap_or(crate::types::QueueMode::All),
        follow_up_mode: options.follow_up_mode.unwrap_or(crate::types::QueueMode::All),
        tool_execution: options.tool_execution.unwrap_or(crate::harness::session::types::ToolExecutionMode::Parallel),
        tool_context: None,
        system_prompt: options.system_prompt.clone(),
        to_provider_messages: Arc::new(|messages, _context| {
            Box::pin(async move { crate::harness::messages::convert_to_llm(messages) })
        }),
        entry_projectors: Default::default(),
    };
    let harness = Harness {
        session: options.session.clone(),
        events: HarnessEventBus::new(),
        models: options.models.clone(),
        hooks: Arc::new(HookRegistry::new(noop_hook_reporter())),
        lanes_by_name: Mutex::new(BTreeMap::new()),
        seed,
        closed_error: Mutex::new(None),
        close_lock: tokio::sync::Mutex::new(()),
        config: Arc::new(Mutex::new(config)),
        session_closed: Mutex::new(false),
    };
    let restored = crate::harness::runtime::restore::restore_session(options.session.as_ref(), context).await?;
    let open: Vec<OpenOperation> = restored
        .iter()
        .filter_map(|(lane, state)| {
            state.operation.as_ref().map(|operation| OpenOperation {
                lane: lane.clone(),
                operation_id: operation.meta.operation_id.clone(),
                kind: operation.meta.intent.kind(),
                started_at: operation.meta.started_at,
                aborting: matches!(operation.state.operation_scope_of().control, Control::CancelRequested { .. }),
            })
        })
        .collect();
    {
        let mut lanes = harness.lanes_by_name.lock().unwrap_or_else(|error| error.into_inner());
        for (name, state) in restored {
            let lane = harness.build_lane(&name, state);
            lanes.insert(name, lane);
        }
    }
    Ok((harness, open))
}
