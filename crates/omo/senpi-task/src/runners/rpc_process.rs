//! `runners/rpc-process.ts`: spawn a senpi RPC child (never through a shell) with an isolated
//! session dir and return a steerable handle. Process destruction goes only through
//! `rpc/terminate.rs`.

use regex::Regex;
use std::sync::LazyLock;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

use serde_json::json;

use crate::runners::rpc::handle::{Clock, RpcChildHandle, RpcChildHandleOptions};
use crate::runners::rpc::model_admission::{
    RpcModelAdmission, RpcModelAdmissionOptions, create_rpc_model_admission,
};
use crate::runners::rpc::process::{RpcChildProcess, RpcSpawnDescriptor};
use crate::runners::rpc::protocol_client::{
    MalformedLineHandler, RpcClientPort, RpcProtocolClient, RpcProtocolClientOptions,
};
use crate::runners::rpc::spawn::{RpcSpawnRuntime, build_rpc_spawn};
use crate::runners::types::{ChildExitOutcome, RpcRunnerSpec, RpcSpawnSpec};
use crate::runners::{RunnerFailure, RunnerFailureKind};

const DEFAULT_HEARTBEAT_INTERVAL_MS: u64 = 10_000;

pub type SpawnChild = Arc<dyn Fn(&RpcSpawnDescriptor) -> Arc<RpcChildProcess> + Send + Sync>;
pub type BuildSpawn = Arc<dyn Fn(&RpcRunnerSpec) -> RpcSpawnDescriptor + Send + Sync>;

#[derive(Clone, Default)]
pub struct RpcProcessRunnerOptions {
    pub fallback_chain_deadline_ms: Option<u64>,
    pub exclude_models: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pub spawn_child: Option<SpawnChild>,
    pub build_spawn: Option<BuildSpawn>,
    pub heartbeat_interval_ms: Option<u64>,
    pub on_malformed_line: Option<MalformedLineHandler>,
    pub now: Option<Clock>,
    pub model_admission: Option<RpcModelAdmission>,
    pub inherited_extensions: Vec<String>,
}

pub struct RpcProcessRunner {
    fallback_chain_deadline_ms: u64,
    unsupported_noticed: std::sync::atomic::AtomicBool,
    exclude_models: Option<Arc<dyn Fn() -> Vec<String> + Send + Sync>>,
    pending_teardowns: Mutex<Vec<JoinHandle<()>>>,
    spawn_child: SpawnChild,
    build_spawn: BuildSpawn,
    heartbeat_interval_ms: u64,
    on_malformed_line: Option<MalformedLineHandler>,
    now: Clock,
    model_admission: RpcModelAdmission,
    inherited_extensions: Vec<String>,
}

impl RpcProcessRunner {
    pub fn new(options: RpcProcessRunnerOptions) -> Self {
        Self {
            fallback_chain_deadline_ms: options.fallback_chain_deadline_ms.unwrap_or(10_000),
            unsupported_noticed: std::sync::atomic::AtomicBool::new(false),
            exclude_models: options.exclude_models,
            pending_teardowns: Mutex::new(Vec::new()),
            spawn_child: options.spawn_child.unwrap_or_else(|| {
                Arc::new(|descriptor| Arc::new(RpcChildProcess::spawn(descriptor)))
            }),
            build_spawn: options.build_spawn.unwrap_or_else(|| {
                let runtime = RpcSpawnRuntime::default();
                Arc::new(move |spec| build_rpc_spawn(spec, &runtime))
            }),
            heartbeat_interval_ms: options
                .heartbeat_interval_ms
                .unwrap_or(DEFAULT_HEARTBEAT_INTERVAL_MS),
            on_malformed_line: options.on_malformed_line,
            now: options
                .now
                .unwrap_or_else(|| Arc::new(|| chrono::Utc::now().timestamp_millis())),
            model_admission: options
                .model_admission
                .unwrap_or_else(|| create_rpc_model_admission(RpcModelAdmissionOptions::default())),
            inherited_extensions: options.inherited_extensions,
        }
    }

    pub fn settle_teardowns(&self) {
        let pending = std::mem::take(
            &mut *self
                .pending_teardowns
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        for teardown in pending {
            if let Err(error) = teardown.join() {
                utils::logger::log(
                    "senpi-task rpc start cleanup panicked",
                    Some(&json!({"error":format!("{error:?}")})),
                );
            }
        }
    }

    /// Admit the model, spawn, then either drive the initial prompt or switch to the resume session.
    /// Failed non-resume starts discard their child in a tracked background teardown.
    pub fn start(&self, spec_input: &RpcRunnerSpec) -> Result<Arc<RpcChildHandle>, RunnerFailure> {
        let mut spec = spec_input.clone();
        if let Some(resolve) = &self.exclude_models
            && let Some(chain) = &spec.fallback_models {
                spec.fallback_models = Some(crate::manager::runner::without_excluded_selectors(
                    chain,
                    &resolve(),
                ));
            }
        if spec.extensions.is_none() && !self.inherited_extensions.is_empty() {
            spec.extensions = Some(self.inherited_extensions.clone());
        }
        (self.model_admission)(&spec)?;
        let descriptor = (self.build_spawn)(&spec);
        let child = (self.spawn_child)(&descriptor);
        let protocol = Arc::new(RpcProtocolClient::new(
            child,
            RpcProtocolClientOptions {
                on_malformed_line: self.on_malformed_line.clone(),
                auto_answer_ui: None,
            },
        ));
        let client: Arc<dyn RpcClientPort> = protocol.clone();
        let handle = RpcChildHandle::new(RpcChildHandleOptions {
            client: Arc::clone(&client),
            task_id: spec.task_id.clone(),
            heartbeat_interval_ms: self.heartbeat_interval_ms,
            now: Arc::clone(&self.now),
        });
        if let (Some(model), Some(chain)) = (&spec.model, &spec.fallback_models)
            && !chain.is_empty() {
                let deadline = std::time::Instant::now()
                    + std::time::Duration::from_millis(self.fallback_chain_deadline_ms);
                let configured = protocol.request(json!({"type":"get_protocol_info"}))
                    .and_then(|pending| pending.wait_until(deadline))
                    .and_then(|response| {
                        let supported = response["success"] == true && response["command"] == "get_protocol_info" && response["data"]["capabilities"].as_array().is_some_and(|items| items.iter().any(|item| item == "retry_fallback_command"));
                        if !supported {
                            if !self.unsupported_noticed.swap(true, std::sync::atomic::Ordering::AcqRel) {
                                utils::logger::log("senpi-task engine lacks retry_fallback_command; retaining manager fallback", None);
                            }
                            return Ok(());
                        }
                        if std::time::Instant::now() >= deadline {
                            return Err(crate::runners::rpc::protocol_client::RpcClientError::Write { message: "fallback configuration deadline passed".into() });
                        }
                        protocol.request(json!({"type":"set_retry_fallback","retryFallback":{"modelFallback":true,"fallbackChains":{(model):chain}}}))?.wait_until(deadline).and_then(|response| {
                            if response["success"] == true { Ok(()) } else { Err(crate::runners::rpc::protocol_client::RpcClientError::Write { message: format!("fallback configuration refused: {}", response["error"]) }) }
                        })
                    });
                if let Err(error) = configured {
                    utils::logger::log(
                        "senpi-task child fallback configuration unavailable; retaining manager fallback",
                        Some(&json!({"taskId":spec.task_id,"error":error.to_string()})),
                    );
                }
            }
        let started = match &spec.resume_session_path {
            None => handle
                .start_initial_prompt(&spec.prompt)
                .map(|()| None)
                .map_err(|error| (RunnerFailureKind::ChildPromptFailed, error.to_string())),
            Some(path) => client
                .switch_session(path)
                .map(|result| Some((path.clone(), result)))
                .map_err(|error| (RunnerFailureKind::SessionUnavailable, error.to_string())),
        };
        let resume = match started {
            Ok(resume) => resume,
            Err((kind, message)) => {
                let unresolved = match handle.exit_outcome() {
                    Some(ChildExitOutcome::Crashed { facts }) => {
                        child_unresolved_model_error(&facts.stderr_tail)
                    }
                    _ => None,
                };
                let task_id = spec.task_id.clone();
                let cleanup = move || {
                    if let Err(error) =
                        crate::runners::rpc::start_cleanup::discard_unstarted_rpc_handle(handle)
                    {
                        utils::logger::log(
                            "senpi-task rpc start cleanup failed",
                            Some(&json!({"taskId":task_id,"error":error.to_string()})),
                        );
                    }
                };
                if spec.resume_session_path.is_none() {
                    let mut pending = self
                        .pending_teardowns
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    // Retire completed workers without losing panic diagnostics.
                    let mut index = 0;
                    while index < pending.len() {
                        if pending[index].is_finished() {
                            if let Err(error) = pending.swap_remove(index).join() {
                                utils::logger::log(
                                    "senpi-task rpc start cleanup panicked",
                                    Some(&json!({"error":format!("{error:?}")})),
                                );
                            }
                        } else {
                            index += 1;
                        }
                    }
                    pending.push(std::thread::spawn(cleanup));
                } else {
                    cleanup();
                }
                return Err(match unresolved {
                    Some(detail) => RunnerFailure::new(
                        RunnerFailureKind::ModelUnavailable,
                        format!("the child could not resolve its model: {detail}"),
                    ),
                    None => RunnerFailure::new(kind, message),
                });
            }
        };
        handle.attach_start_facts(
            RpcSpawnSpec {
                cwd: spec.cwd.clone(),
                extensions: spec.extensions.clone(),
                member_env: spec.member_env.clone(),
            },
            resume,
        );
        Ok(handle)
    }
}

static ANSI_ESCAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\x1B\[[0-?]*[ -/]*[@-~]").expect("ANSI escape regex"));
static UNRESOLVED_MODEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"Model "[^"\n]+" not found\. Use --list-models to see available models\.|Unknown provider "[^"\n]+"\. Use --list-models to see available providers/models\."#).expect("model error regex")
});
pub fn child_unresolved_model_error(stderr_tail: &str) -> Option<String> {
    let clean = ANSI_ESCAPE.replace_all(stderr_tail, "");
    UNRESOLVED_MODEL
        .find(&clean)
        .map(|found| found.as_str().to_owned())
}
#[cfg(test)]
mod model_error_tests {
    use super::*;
    #[test]
    fn recognizes_only_engine_model_diagnostics() {
        for diagnostic in [
            r#"Model "fixture/missing" not found. Use --list-models to see available models."#,
            r#"Unknown provider "missing". Use --list-models to see available providers/models."#,
        ] {
            assert_eq!(
                child_unresolved_model_error(&format!("noise\n\x1b[31m{diagnostic}\x1b[0m\n")),
                Some(diagnostic.into())
            );
        }
        assert_eq!(child_unresolved_model_error("ECONNRESET"), None);
        assert_eq!(child_unresolved_model_error("Model not found"), None);
    }
}

#[cfg(all(test, unix))]
mod startup_tests {
    use super::*;
    #[test]
    fn crashed_child_model_diagnostics_are_typed_and_teardowns_settle() {
        for (stderr, expected) in [
            (
                r#"Model "fixture/missing" not found. Use --list-models to see available models."#,
                RunnerFailureKind::ModelUnavailable,
            ),
            (
                r#"Unknown provider "fixture". Use --list-models to see available providers/models."#,
                RunnerFailureKind::ModelUnavailable,
            ),
            ("connection reset", RunnerFailureKind::ChildPromptFailed),
        ] {
            let script = format!("read -r command; printf '%s\\n' '{}' >&2; exit 1", stderr);
            let runner = RpcProcessRunner::new(RpcProcessRunnerOptions {
                model_admission: Some(Arc::new(|_| Ok(()))),
                build_spawn: Some(Arc::new(move |_| RpcSpawnDescriptor {
                    command: "/bin/sh".into(),
                    args: vec!["-c".into(), script.clone()],
                    cwd: "/tmp".into(),
                    env: std::env::vars().collect(),
                })),
                ..Default::default()
            });
            let failure = runner
                .start(&RpcRunnerSpec {
                    task_id: "model-start".into(),
                    model: Some("fixture/missing".into()),
                    prompt: "hello".into(),
                    ..Default::default()
                })
                .err()
                .expect("startup rejection");
            assert_eq!(failure.kind, expected);
            runner.settle_teardowns();
            assert!(runner.pending_teardowns.lock().expect("pending").is_empty());
        }
    }
    #[test]
    fn settle_teardowns_waits_for_registered_completion_event() {
        let runner = RpcProcessRunner::new(Default::default());
        let (release, wait) = std::sync::mpsc::channel();
        let (done, completion) = std::sync::mpsc::channel();
        runner
            .pending_teardowns
            .lock()
            .expect("pending")
            .push(std::thread::spawn(move || {
                wait.recv_timeout(std::time::Duration::from_secs(2))
                    .expect("release cleanup");
                done.send(()).expect("completion");
            }));
        release.send(()).expect("release");
        runner.settle_teardowns();
        completion
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("settled");
        assert!(runner.pending_teardowns.lock().expect("pending").is_empty());
    }
}
