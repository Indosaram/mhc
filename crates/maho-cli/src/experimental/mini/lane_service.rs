//! Port of senpi `experimental/mini/worker/lane-service.ts`.
//!
//! The lane, harness, and model registry stay in the worker; each presentation gets its own
//! `lane.watch()`, whose snapshot and event stream the harness pairs with no gap and no duplicate.
//! The real harness `Lane` implements `RuntimeLane` (maho-agent, contract S1), so it is passed
//! directly to `transcript::watch_lane`.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use maho_agent::harness::context::Context;
use maho_agent::harness::events::{BufferedEventWatcher, HarnessEvent};
use maho_agent::harness::runtime::lane::{AdmissionError, Lane, OperationAdmission, PromptInput, QueuedInput};
use maho_agent::harness::runtime::transcript::watch_lane;
use maho_agent::harness::runtime::types::RuntimeLane;
use maho_agent::harness::session::session::SessionError;
use maho_agent::harness::session::types::{LaneModelRef, RunSettings};

use super::runtime::ModelRuntimeHandle;
use super::shared::protocol::{CommandResult, LaneSubscription, ModelRef, ModelsState, SessionSnapshot};

pub struct SessionIdentity {
    pub id: String,
    pub cwd: String,
    pub path: String,
}

pub struct LaneServiceOptions {
    pub lane: Arc<Lane>,
    pub models: Arc<ModelRuntimeHandle>,
    pub context: Context,
    pub session: SessionIdentity,
    pub settings: RunSettings,
    pub models_state: Arc<dyn Fn() -> ModelsState + Send + Sync>,
    pub publish: Arc<dyn Fn(&str, &str, &HarnessEvent) + Send + Sync>,
}

struct Watch {
    watcher: Arc<BufferedEventWatcher<Result<serde_json::Value, SessionError>>>,
    to: String,
}

pub struct LaneService {
    options: LaneServiceOptions,
    watches: Mutex<HashMap<String, Watch>>,
}

impl LaneService {
    pub fn new(options: LaneServiceOptions) -> Self {
        Self { options, watches: Mutex::new(HashMap::new()) }
    }

    pub async fn watch(&self, presentation_id: &str) -> Result<LaneSubscription, String> {
        let lane: Arc<dyn RuntimeLane> = self.options.lane.clone();
        let watcher = watch_lane(lane, self.options.lane.events.clone(), &self.options.context, false)
            .await
            .map_err(|error| error.message)?;
        let snapshot = match watcher.snapshot() {
            Ok(snapshot) => snapshot,
            Err(error) => {
                watcher.unsubscribe();
                return Err(error.message);
            }
        };
        let subscription_id = maho_ai::utils::uuid::uuidv7(None).unwrap_or_default();
        self.watches
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .insert(subscription_id.clone(), Watch { watcher, to: presentation_id.to_owned() });
        Ok(LaneSubscription {
            subscription_id,
            snapshot: SessionSnapshot {
                session_id: self.options.session.id.clone(),
                cwd: self.options.session.cwd.clone(),
                session_path: self.options.session.path.clone(),
                lane: snapshot,
                models: (self.options.models_state)(),
            },
        })
    }

    pub async fn start(&self, subscription_id: &str) -> Result<(), String> {
        let (watcher, to) = {
            let watches = self.watches.lock().unwrap_or_else(|error| error.into_inner());
            let watch = watches.get(subscription_id).ok_or_else(|| format!("Unknown subscription: {subscription_id}"))?;
            (watch.watcher.clone(), watch.to.clone())
        };
        let publish = self.options.publish.clone();
        let subscription = subscription_id.to_owned();
        let listener: maho_agent::harness::events::UntypedEventListener = Arc::new(move |event: HarnessEvent, _context: Context| {
            publish(&subscription, &to, &event);
            Box::pin(async {})
        });
        watcher.start(listener).await;
        Ok(())
    }

    pub fn unwatch(&self, subscription_id: &str) {
        if let Some(watch) = self.watches.lock().unwrap_or_else(|error| error.into_inner()).remove(subscription_id) {
            watch.watcher.unsubscribe();
        }
    }

    pub async fn prompt(&self, text: &str) -> CommandResult {
        let input = PromptInput::Text { text: text.to_owned(), images: vec![] };
        self.admit(self.options.lane.accept_prompt(input, None, self.options.settings.clone(), &self.options.context)).await
    }

    pub async fn steer(&self, text: &str) -> CommandResult {
        self.queue(self.options.lane.steer(QueuedInput::Text(text.to_owned()), vec![], &self.options.context)).await
    }

    pub async fn follow_up(&self, text: &str) -> CommandResult {
        self.queue(self.options.lane.follow_up(QueuedInput::Text(text.to_owned()), vec![], &self.options.context)).await
    }

    pub async fn compact(&self) -> CommandResult {
        self.admit(self.options.lane.accept_compaction(None, None, self.options.settings.clone(), &self.options.context)).await
    }

    pub async fn abort(&self) -> CommandResult {
        let operation_id = match self.options.lane.state().operation {
            Some(operation) => operation.meta.operation_id,
            None => return CommandResult::Error("No active operation to abort".to_owned()),
        };
        match self.options.lane.request_operation_abort(operation_id, &self.options.context).await {
            Ok(Ok(_)) => CommandResult::Ok,
            Ok(Err(mismatch)) => CommandResult::Error(format!(
                "Operation mismatch: expected {}, current {}",
                mismatch.expected,
                mismatch.current_operation_id.unwrap_or_else(|| "none".to_owned())
            )),
            Err(error) => CommandResult::Error(error.message),
        }
    }

    pub async fn set_model(&self, reference: &ModelRef) -> CommandResult {
        if !self.options.models.has_model(&reference.provider, &reference.model_id).await {
            return CommandResult::Error(format!("Unknown model: {}/{}", reference.provider, reference.model_id));
        }
        let model = LaneModelRef { provider: reference.provider.clone(), model_id: reference.model_id.clone() };
        match self.options.lane.set_model(model, &self.options.context).await {
            Ok(()) => CommandResult::Ok,
            Err(error) => CommandResult::Error(error.message),
        }
    }

    pub fn close(&self) {
        let mut watches = self.watches.lock().unwrap_or_else(|error| error.into_inner());
        for watch in watches.values() {
            watch.watcher.unsubscribe();
        }
        watches.clear();
    }

    async fn admit<F>(&self, future: F) -> CommandResult
    where
        F: Future<Output = Result<Result<OperationAdmission, AdmissionError>, SessionError>>,
    {
        match future.await {
            Ok(Ok(_)) => CommandResult::Ok,
            Ok(Err(error)) => CommandResult::Error(error.to_string()),
            Err(error) => CommandResult::Error(error.message),
        }
    }

    async fn queue<F>(&self, future: F) -> CommandResult
    where
        F: Future<Output = Result<String, SessionError>>,
    {
        match future.await {
            Ok(_) => CommandResult::Ok,
            Err(error) => CommandResult::Error(error.message),
        }
    }
}
