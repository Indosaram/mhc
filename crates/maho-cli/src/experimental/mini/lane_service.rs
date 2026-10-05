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
use maho_agent::harness::runtime::lane::{Lane, NavigationOptions, PromptInput, QueuedInput};
use maho_agent::harness::runtime::transcript::watch_lane;
use maho_agent::harness::runtime::types::RuntimeLane;
use maho_agent::harness::session::session::SessionError;
use maho_agent::harness::session::types::LaneModelRef;

use super::runtime::ModelRuntimeHandle;
use super::shared::protocol::{CommandResult, LaneSubscription, ModelRef, ModelsState, SessionSnapshot};

/// The worker's lane-event publisher: `(subscriptionId, presentationId, event)`.
pub type LaneEventPublisher = Arc<dyn Fn(&str, &str, &HarnessEvent) + Send + Sync>;

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
    pub models_state: Arc<dyn Fn() -> ModelsState + Send + Sync>,
    pub publish: LaneEventPublisher,
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
        self.run(self.options.lane.prompt(input, &self.options.context)).await
    }

    pub async fn skill(&self, name: &str, additional_instructions: Option<String>) -> CommandResult {
        self.run(self.options.lane.skill(name, additional_instructions, &self.options.context)).await
    }

    pub async fn steer(&self, text: &str) -> CommandResult {
        self.queue(self.options.lane.steer(QueuedInput::Text(text.to_owned()), vec![], &self.options.context)).await
    }

    pub async fn follow_up(&self, text: &str) -> CommandResult {
        self.queue(self.options.lane.follow_up(QueuedInput::Text(text.to_owned()), vec![], &self.options.context)).await
    }

    pub async fn compact(&self) -> CommandResult {
        self.run(self.options.lane.compact(None, &self.options.context)).await
    }

    pub async fn navigate_tree(&self, target_id: Option<String>, options: NavigationOptions) -> CommandResult {
        self.run(self.options.lane.navigate_tree(target_id, options, &self.options.context)).await
    }

    pub async fn abort(&self) -> CommandResult {
        self.run(self.options.lane.abort(&self.options.context)).await
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

    async fn run<F, O, E>(&self, future: F) -> CommandResult
    where
        F: Future<Output = Result<Result<O, E>, SessionError>>,
        E: std::fmt::Display,
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
