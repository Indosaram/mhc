//! Production hosted session connection and exact-turn tracking.
use senpi_task::runners::rpc_host::{
    host_command_dispatch::HostRpcClient, task_parent_wire::HostRunHoldClient,
};
use senpi_task::{
    host::HostError,
    runners::{RunnerFailureKind, RunnerOutcome},
};
use serde_json::{Value, json};
use std::sync::{Arc, Condvar, Mutex};

fn failure(error: impl std::fmt::Display) -> HostError {
    HostError {
        message: error.to_string(),
    }
}
pub type TurnState = Arc<(Mutex<(u64, Option<RunnerOutcome>)>, Condvar)>;

pub struct LiveClient {
    pub client: Arc<maho_rpc::rpc_client::RpcClient>,
    pub runtime: tokio::runtime::Handle,
    turn: Arc<(Mutex<(u64, Option<RunnerOutcome>)>, Condvar)>,
    subscription: Mutex<Option<maho_rpc::rpc_client::RpcClientUnsubscribe>>,
}
impl LiveClient {
    pub fn new(
        client: Arc<maho_rpc::rpc_client::RpcClient>,
        runtime: tokio::runtime::Handle,
        turn: TurnState,
    ) -> Arc<Self> {
        let observed = turn.clone();
        let subscription = client.on_event(Arc::new(move |event| {
            let mut state = observed.0.lock().unwrap();
            if event["type"] == "agent_start" {
                state.0 += 1;
                state.1 = None;
            }
            if event["type"] == "agent_end" && event["willRetry"] != true {
                state.1 = Some(senpi_task::runners::rpc::turn_outcome::agent_end_outcome(
                    event, None, None,
                ));
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
        let mut state = self.turn.0.lock().unwrap();
        while state.1.is_none() {
            state = self.turn.1.wait(state).unwrap();
        }
        state.1.clone().unwrap()
    }
    pub fn watch(
        self: &Arc<Self>,
    ) -> senpi_task::runners::in_process::native_run_pause::EngineTurnWatch {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let started = std::sync::atomic::AtomicBool::new(false);
        let subscription = self.client.on_event(Arc::new(move |event| {
            if event["type"] == "agent_start" {
                started.store(true, std::sync::atomic::Ordering::Release);
            }
            if started.load(std::sync::atomic::Ordering::Acquire)
                && event["type"] == "agent_end"
                && event["willRetry"] != true
            {
                let _ = tx.try_send(senpi_task::runners::rpc::turn_outcome::agent_end_outcome(
                    event, None, None,
                ));
            }
        }));
        let subscription = Arc::new(Mutex::new(Some(subscription)));
        let cancel = subscription.clone();
        senpi_task::runners::in_process::native_run_pause::EngineTurnWatch {
            outcome: Box::new(move || {
                let outcome = rx.recv().unwrap_or_else(|_| {
                    RunnerOutcome::error(
                        RunnerFailureKind::ChildTurnFailed,
                        "host turn observer closed",
                    )
                });
                subscription.lock().unwrap().take();
                outcome
            }),
            cancel: Box::new(move || {
                cancel.lock().unwrap().take();
            }),
        }
    }
}
impl HostRpcClient for LiveClient {
    fn wait_for_outcome(&self) -> Option<RunnerOutcome> {
        Some(self.outcome())
    }
    fn transport_lost(&self, message: String) {
        self.turn.0.lock().unwrap().1 = Some(RunnerOutcome::error(
            RunnerFailureKind::ChildTurnFailed,
            message,
        ));
        self.turn.1.notify_all();
    }
    fn command(&self, command: Value) -> Result<(), HostError> {
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
impl Drop for LiveClient {
    fn drop(&mut self) {
        self.subscription.get_mut().unwrap().take();
    }
}
