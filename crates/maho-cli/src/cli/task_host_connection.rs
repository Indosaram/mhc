//! Production hosted session connection and exact-turn tracking.
#[cfg(unix)]
use senpi_task::runners::rpc_host::{
    host_command_dispatch::HostRpcClient, task_parent_wire::HostRunHoldClient,
};
#[cfg(unix)]
use senpi_task::{
    host::HostError,
    runners::{RunnerFailureKind, RunnerOutcome},
};
#[cfg(unix)]
use serde_json::{Value, json};
use std::sync::{Arc, Condvar, Mutex};

#[cfg(unix)]
fn failure(error: impl std::fmt::Display) -> HostError {
    HostError {
        message: error.to_string(),
    }
}
#[derive(Default)]
pub struct TurnData {
    #[cfg(unix)]
    generation: u64,
    #[cfg(unix)]
    outcome: Option<RunnerOutcome>,
    #[cfg(unix)]
    next_watch: u64,
    #[cfg(unix)]
    watches: std::collections::BTreeMap<u64, (bool, std::sync::mpsc::SyncSender<RunnerOutcome>)>,
}
pub type TurnState = Arc<(Mutex<TurnData>, Condvar)>;

#[cfg(unix)]
pub struct LiveClient {
    pub client: Arc<maho_rpc::rpc_client::RpcClient>,
    pub runtime: tokio::runtime::Handle,
    turn: TurnState,
    subscription: Mutex<Option<maho_rpc::rpc_client::RpcClientUnsubscribe>>,
}
#[cfg(unix)]
impl LiveClient {
    pub fn new(
        client: Arc<maho_rpc::rpc_client::RpcClient>,
        runtime: tokio::runtime::Handle,
        turn: TurnState,
    ) -> Arc<Self> {
        let observed = turn.clone();
        let subscription = client.on_event(Arc::new(move |event| {
            let mut state = observed.0.lock().expect("host turn state lock poisoned");
            if event["type"] == "agent_start" {
                state.generation += 1;
                state.outcome = None;
                for (started, _) in state.watches.values_mut() {
                    *started = true;
                }
            }
            if event["type"] == "agent_end" && event["willRetry"] != true {
                let outcome =
                    senpi_task::runners::rpc::turn_outcome::agent_end_outcome(event, None, None);
                state.outcome = Some(outcome.clone());
                state.watches.retain(|_, (started, tx)| {
                    if *started {
                        let _ = tx.try_send(outcome.clone());
                        false
                    } else {
                        true
                    }
                });
                observed.1.notify_all();
            }
        }));
        Arc::new(Self {
            client,
            runtime,
            turn,
            subscription: Mutex::new(Some(subscription)),
        })
    }
    pub fn call<T: Send + 'static>(
        &self,
        future: impl std::future::Future<Output = Result<T, maho_rpc::rpc_client::RpcClientError>>
        + Send
        + 'static,
    ) -> Result<T, HostError> {
        let runtime = self.runtime.clone();
        std::thread::spawn(move || runtime.block_on(future))
            .join()
            .map_err(|_| failure("host client thread panicked"))?
            .map_err(failure)
    }
    pub fn outcome(&self) -> RunnerOutcome {
        let mut state = self.turn.0.lock().expect("host turn state lock poisoned");
        while state.outcome.is_none() {
            state = self.turn.1.wait(state).expect("host turn state lock poisoned");
        }
        state.outcome.clone().expect("completed host turn has an outcome")
    }
    pub fn watch(
        self: &Arc<Self>,
    ) -> senpi_task::runners::in_process::native_run_pause::EngineTurnWatch {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let token = {
            let mut state = self.turn.0.lock().expect("host turn state lock poisoned");
            state.next_watch += 1;
            let token = state.next_watch;
            state.watches.insert(token, (false, tx));
            token
        };
        let cancel = self.turn.clone();
        senpi_task::runners::in_process::native_run_pause::EngineTurnWatch {
            outcome: Box::new(move || {

                rx.recv().unwrap_or_else(|_| {
                    RunnerOutcome::error(
                        RunnerFailureKind::ChildTurnFailed,
                        "host turn observer closed",
                    )
                })
            }),
            cancel: Box::new(move || {
                cancel.0.lock().expect("host turn state lock poisoned").watches.remove(&token);
            }),
        }
    }
}
#[cfg(unix)]
impl HostRpcClient for LiveClient {
    fn get_available_models(
        &self,
    ) -> Result<Vec<senpi_task::runners::pinned_model_equivalence::CatalogModelIdentity>, HostError>
    {
        let client = self.client.clone();
        let value = self.call(async move { client.get_available_models().await })?;
        serde_json::from_value(value.get("models").cloned().unwrap_or(value)).map_err(failure)
    }
    fn wait_for_outcome(&self) -> Option<RunnerOutcome> {
        Some(self.outcome())
    }
    fn transport_lost(&self, message: String) {
        let mut state = self.turn.0.lock().expect("host turn state lock poisoned");
        let outcome = RunnerOutcome::error(RunnerFailureKind::ChildTurnFailed, message);
        state.outcome = Some(outcome.clone());
        for (_, (_, tx)) in std::mem::take(&mut state.watches) {
            let _ = tx.try_send(outcome.clone());
        }
        drop(state);
        self.turn.1.notify_all();
    }
    fn command(&self, command: Value) -> Result<(), HostError> {
        if matches!(
            command["type"].as_str(),
            Some("prompt" | "followUp" | "steer")
        ) {
            self.turn.0.lock().expect("host turn state lock poisoned").outcome = None;
        }
        let client = self.client.clone();
        self.call(async move {
            match command["type"].as_str().unwrap_or_default() {
                "prompt" => {
                    client
                        .prompt(command["message"].as_str().unwrap_or_default(), json!({}))
                        .await
                }
                "steer" => {
                    client
                        .steer(command["message"].as_str().unwrap_or_default(), None, None)
                        .await
                }
                "followUp" => {
                    client
                        .follow_up(command["message"].as_str().unwrap_or_default(), None, None)
                        .await
                }
                "abort" => client.abort().await,
                "set_session_context" => {
                    let patch = serde_json::from_value(command["patch"].clone()).map_err(|e| {
                        maho_rpc::rpc_client::RpcClientError::Transport(std::io::Error::other(e))
                    })?;
                    client.set_session_context(patch).await.map(|_| ())
                }
                _ => Err(std::io::Error::other("unsupported hosted command").into()),
            }
        })
    }
    fn get_state(&self) -> Result<Value, HostError> {
        let client = self.client.clone();
        self.call(async move { client.get_state().await })
    }
    fn on_event(
        &self,
        listener: senpi_task::manager::child_handle::ManagedChildListener,
    ) -> senpi_task::manager::child_handle::Unsubscribe {
        let subscription = self.client.on_event(Arc::new(move |event| {
            let event = senpi_task::runners::rpc::handle::managed_event(event);
            listener(&event);
        }));
        Box::new(move || drop(subscription))
    }
    fn close(&self, _: &str) -> Result<(), HostError> {
        let client = self.client.clone();
        self.call(async move { client.close_session(None).await })
    }
    fn detach(&self, _: &str) -> Result<(), HostError> {
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        std::thread::spawn(move || runtime.block_on(client.stop()))
            .join()
            .map_err(|_| failure("host detach panicked"))
    }
}
#[cfg(unix)]
impl HostRunHoldClient for LiveClient {
    fn set_client_info(&self, _: &Value) -> Option<Result<Value, String>> {
        let client = self.client.clone();
        Some(
            self.call(async move {
                client
                    .set_client_info(80., Some(vec!["task_parent".into()]))
                    .await
                    .map(|_| json!({}))
            })
            .map_err(|e| e.to_string()),
        )
    }
    fn get_state(&self) -> Option<Result<Value, String>> {
        Some(HostRpcClient::get_state(self).map_err(|e| e.to_string()))
    }
    fn pause_for_run(&self, command: &Value) -> Option<Result<Value, String>> {
        let c = command.clone();
        let client = self.client.clone();
        Some(
            self.call(async move {
                client
                    .pause_for_run(
                        c["parentSessionId"].as_str().unwrap_or_default(),
                        c["parentPauseSeq"].as_u64().unwrap_or_default(),
                        maho_core::run_holds::RunRef {
                            kind: c["runRef"]["kind"].as_str().unwrap_or_default().into(),
                            id: c["runRef"]["id"].as_str().unwrap_or_default().into(),
                        },
                    )
                    .await
            })
            .map_err(|e| e.to_string()),
        )
    }
    fn resume_for_run(&self, command: &Value) -> Option<Result<Value, String>> {
        let c = command.clone();
        let client = self.client.clone();
        Some(
            self.call(async move {
                client
                    .resume_for_run(
                        c["parentSessionId"].as_str().unwrap_or_default(),
                        c["parentPauseSeq"].as_u64().unwrap_or_default(),
                        c["resumeId"].as_str().unwrap_or_default(),
                        c["childSeqCutoff"].as_u64().unwrap_or_default(),
                    )
                    .await
            })
            .map_err(|e| e.to_string()),
        )
    }
    fn hold_input(&self, command: &Value) -> Option<Result<Value, String>> {
        let c = command.clone();
        let client = self.client.clone();
        Some(
            self.call(async move {
                client
                    .hold_input(
                        c["inputId"].as_str().unwrap_or_default(),
                        c["revision"].as_u64().unwrap_or_default(),
                        c["text"].as_str().unwrap_or_default(),
                        c["parentSessionId"].as_str(),
                    )
                    .await
            })
            .map_err(|e| e.to_string()),
        )
    }
}
#[cfg(unix)]
impl Drop for LiveClient {
    fn drop(&mut self) {
        self.subscription.get_mut().expect("host subscription lock poisoned").take();
    }
}
