use crate::context::MemoryIdentityContext;
use memory_core::{
    identity::resolve::{MemoryIdentity, sanitize_to_slug},
    journal::store::{TranscriptJournal, TranscriptJournalOptions},
    reflection::{ReflectionReservationStore, ReflectionReservationStoreOptions},
};
use std::sync::Arc;
#[derive(Default)]
pub struct IdentitySandbox {
    built: Option<crate::sandbox::SandboxTransform>,
}
pub use maho_omo_agent_home::resolve_agent_home::AGENT_DIR_ENV_NAMES;
pub struct IdentitySandboxInput<'a> {
    pub identity: &'a MemoryIdentityContext,
    pub policy: crate::sandbox::SandboxPolicy,
    pub agent_dir: &'a std::path::Path,
    pub platform: &'a str,
}
impl IdentitySandbox {
    pub fn apply(
        &mut self,
        input: IdentitySandboxInput<'_>,
        mut args: crate::worker::spawn_types::ReflectionSpawnArgs,
        which: &dyn Fn(&str) -> Option<String>,
        warn: impl FnOnce(&str),
    ) -> Result<crate::worker::spawn_types::ReflectionSpawnArgs, String> {
        let IdentitySandboxInput {
            identity,
            policy,
            agent_dir,
            platform,
        } = input;
        if self.built.is_none() {
            let mut writes = vec![
                identity.identity_paths.reflection_sessions.clone(),
                identity.identity_paths.reflection.clone(),
                agent_dir.into(),
            ];
            if let Some(config) = args.env.get("XDG_CONFIG_HOME") {
                writes.push(config.into());
            }
            let transform = crate::sandbox::build_sandbox_transform(
                &crate::sandbox::ReflectionSandboxInput {
                    policy,
                    worktree_dir: &identity.identity_paths.worktrees,
                    git_common_dir: &identity.identity_paths.repo,
                    payload_paths: std::slice::from_ref(&identity.identity_paths.transcripts),
                    runtime_writes: &writes,
                    foreign_roots: &[],
                    command: &args.command,
                    env: &args.env,
                    platform,
                },
                which,
            )
            .map_err(|error| error.to_string())?;
            if let Some(warning) = &transform.warning {
                warn(warning);
            }
            self.built = Some(transform);
        }
        let transformed = self
            .built
            .as_ref()
            .ok_or("Missing identity sandbox")?
            .apply(crate::sandbox_contracts::SandboxSpawnArgs {
                command: args.command,
                args: args.args,
                cwd: args.cwd,
                env: args.env,
            });
        args.command = transformed.command;
        args.args = transformed.args;
        args.cwd = transformed.cwd;
        args.env = transformed.env;
        for name in AGENT_DIR_ENV_NAMES {
            args.env
                .insert(name.into(), agent_dir.to_string_lossy().into_owned());
        }
        Ok(args)
    }
}

pub fn as_memory_identity(context: &MemoryIdentityContext) -> MemoryIdentity {
    MemoryIdentity {
        id: context.identity.clone(),
        safe_slug: sanitize_to_slug(&context.identity),
        paths: context.identity_paths.clone(),
    }
}
pub struct MemoryIdentityRuntime {
    pub identity: MemoryIdentityContext,
    pub store: Arc<ReflectionReservationStore>,
    pub runner: crate::worker::runner::SenpiSubprocessRunner,
    pub sandbox: IdentitySandbox,
    /// The one identity-wide reconcile in flight (upstream `reconciliation`).
    reconciliation: std::sync::Mutex<Option<ReconcileFlight>>,
}
type ReconcileOutcome = Result<Vec<(String, String)>, String>;
/// The shared flight: its first observer owns cancellation; others wait for its outcome.
struct ReconcileFlight {
    id: u64,
    signal: Option<maho_ext_api::AbortSignal>,
    done: tokio::sync::watch::Receiver<Option<ReconcileOutcome>>,
}
struct ClearFlight<'a> {
    slot: &'a std::sync::Mutex<Option<ReconcileFlight>>,
    id: u64,
}
impl Drop for ClearFlight<'_> {
    fn drop(&mut self) {
        let mut slot = self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if slot.as_ref().is_some_and(|flight| flight.id == self.id) {
            *slot = None;
        }
    }
}
static NEXT_FLIGHT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub fn create_identity_runtime(
    identity: MemoryIdentityContext,
    settings: &serde_json::Value,
) -> Result<MemoryIdentityRuntime, String> {
    let store = create_identity_reservation_store(&identity, settings)?;
    let mut runner = crate::worker::runner::SenpiSubprocessRunner::default();
    runner.admission_memory_root = identity
        .durable_root
        .parent()
        .and_then(std::path::Path::parent)
        .map(std::path::Path::to_path_buf);
    Ok(MemoryIdentityRuntime {
        identity,
        store,
        runner,
        sandbox: Default::default(),
        reconciliation: Default::default(),
    })
}
impl MemoryIdentityRuntime {
    /// A runtime over an existing reservation store (the CLI worker shares one store per identity).
    pub fn with_store(
        identity: MemoryIdentityContext,
        store: Arc<ReflectionReservationStore>,
    ) -> Self {
        Self {
            identity,
            store,
            runner: Default::default(),
            sandbox: Default::default(),
            reconciliation: Default::default(),
        }
    }

    pub async fn launch<
        F: std::future::Future<
                Output = Result<crate::worker::runner_types::ReflectionRunResult, String>,
            >,
    >(
        &self,
        mut run: memory_core::reflection::ReservedRun,
        mut launch: impl FnMut(memory_core::reflection::ReservedRun) -> F,
        warn: impl FnOnce(&str),
    ) {
        loop {
            // Every launch runs in a scope bound to its own run (pin `identity-runtime.ts`): the
            // trigger's scope survives only when it names this run's conversations, so launch facts
            // never bind to whichever session happens to be current.
            let scope = match crate::launch_facts::current_launch_scope() {
                Some(scope)
                    if crate::launch_facts::same_conversations(
                        &scope.conversation_ids,
                        &run.request.conversation_ids,
                    ) =>
                {
                    scope
                }
                _ => crate::launch_facts::LaunchScope {
                    conversation_ids: run.request.conversation_ids.clone(),
                    snapshot: None,
                },
            };
            match crate::launch_facts::with_launch_scope(&scope, launch(run)).await {
                Ok(result) => match result.launch {
                    Some(next) => run = next,
                    None => return,
                },
                Err(error) => {
                    warn(&error);
                    return;
                }
            }
        }
    }
    pub async fn reconcile(
        &self,
        now: &dyn Fn() -> i64,
        recovery: &dyn crate::worker::run_reconciliation::ReflectionRecoveryPort,
        launch: &dyn Fn(&memory_core::reflection::ReservedRun),
    ) -> Result<Vec<(String, String)>, String> {
        self.reconcile_with_signal(now, recovery, launch, None)
            .await
    }
    /// One identity-wide pass, including any already-admitted durable transition. Its first observer
    /// owns cancellation; an independent caller retries a foreign abort once. The flight is never
    /// cleared on timeout alone: a committed transition may still be finishing.
    pub async fn reconcile_with_signal(
        &self,
        now: &dyn Fn() -> i64,
        recovery: &dyn crate::worker::run_reconciliation::ReflectionRecoveryPort,
        launch: &dyn Fn(&memory_core::reflection::ReservedRun),
        signal: Option<&maho_ext_api::AbortSignal>,
    ) -> Result<Vec<(String, String)>, String> {
        use crate::worker::run_liveness::{OBSERVATION_ABORTED, throw_if_aborted};
        let mut attempt = 0;
        loop {
            throw_if_aborted(signal)?;
            // Join the flight in progress, or become its driver.
            let joined = {
                let mut slot = self
                    .reconciliation
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match slot.as_ref() {
                    Some(flight) => Err((flight.done.clone(), flight.signal.clone())),
                    None => {
                        let (publish, done) = tokio::sync::watch::channel(None);
                        let id = NEXT_FLIGHT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        *slot = Some(ReconcileFlight {
                            id,
                            signal: signal.cloned(),
                            done,
                        });
                        Ok((id, publish))
                    }
                }
            };
            let outcome = match joined {
                Ok((id, publish)) => {
                    // Clears the flight even when the driver future is dropped mid-pass; joiners then
                    // see the closed channel as a cancelled flight.
                    let _clear = ClearFlight {
                        slot: &self.reconciliation,
                        id,
                    };
                    let outcome = self.drive_reconcile(now, recovery, launch, signal).await;
                    drop(_clear);
                    let _ = publish.send(Some(outcome.clone()));
                    // The flight's owner reports what the pass did: a failure is not masked by its abort.
                    outcome.as_ref().map_err(Clone::clone)?;
                    outcome
                }
                Err((mut done, flight_signal)) => {
                    let outcome = match done.wait_for(Option::is_some).await {
                        Ok(value) => value
                            .clone()
                            .unwrap_or_else(|| Err(OBSERVATION_ABORTED.to_owned())),
                        // The driver was dropped before publishing: it was cancelled, not finished.
                        Err(_) => Err(OBSERVATION_ABORTED.to_owned()),
                    };
                    // A foreign observer's abort is not this caller's: retry once as its own flight.
                    let foreign_abort = outcome
                        .as_ref()
                        .is_err_and(|error| error == OBSERVATION_ABORTED)
                        && flight_signal
                            .as_ref()
                            .is_some_and(maho_ext_api::AbortSignal::is_aborted)
                        && signal.map(|own| own.is_aborted()) != Some(true);
                    if foreign_abort && attempt == 0 {
                        attempt += 1;
                        continue;
                    }
                    outcome
                }
            };
            throw_if_aborted(signal)?;
            return outcome;
        }
    }
    async fn drive_reconcile(
        &self,
        now: &dyn Fn() -> i64,
        recovery: &dyn crate::worker::run_reconciliation::ReflectionRecoveryPort,
        launch: &dyn Fn(&memory_core::reflection::ReservedRun),
        signal: Option<&maho_ext_api::AbortSignal>,
    ) -> Result<Vec<(String, String)>, String> {
        let identity = as_memory_identity(&self.identity);
        crate::worker::run_reconciliation::reconcile_reflection_runs_with_signal(
            &crate::worker::run_finalization_types::RunFinalizationContext {
                identity: &identity,
                reservation: self.store.as_ref(),
                launch: Some(launch),
                now_ms: now,
                receipts: None,
                warn: None,
            },
            recovery,
            None,
            true,
            signal,
        )
        .await
    }
}
pub fn create_identity_reservation_store(
    context: &MemoryIdentityContext,
    settings: &serde_json::Value,
) -> Result<Arc<ReflectionReservationStore>, String> {
    let config = crate::trigger_wiring::resolve_reflection_trigger_config(
        settings,
        Some(&context.identity),
    )?
    .config;
    let transcripts = context.identity_paths.transcripts.clone();
    Ok(Arc::new(ReflectionReservationStore::new(
        ReflectionReservationStoreOptions {
            identity: as_memory_identity(context),
            config,
            get_journal: Arc::new(move |conversation| {
                Ok(TranscriptJournal::new(TranscriptJournalOptions::new(
                    transcripts.join(conversation),
                )))
            }),
            create_run_id: Some(crate::reflection_run_id::create_reflection_run_id_factory(
                context.identity_paths.reflection.clone(),
                context.identity_paths.reflection_sessions.clone(),
                context.identity_paths.worktrees.clone(),
            )),
            now_iso: None,
            launcher_identity: None,
        },
    )))
}
impl crate::worker::runner_types::ReflectionReservationPort for ReflectionReservationStore {
    fn read_state(&self) -> Result<memory_core::reflection::ReservationState, String> {
        ReflectionReservationStore::read_state(self).map_err(|error| error.to_string())
    }
    fn read_state_with_wait(
        &self,
        wait_timeout_ms: Option<u64>,
    ) -> Result<memory_core::reflection::ReservationState, memory_core::reflection::ReservationError>
    {
        ReflectionReservationStore::read_state_with_wait(self, wait_timeout_ms)
    }
    fn complete(
        &self,
        run: &str,
        outcome: memory_core::reflection::ReflectionOutcome,
    ) -> Result<memory_core::reflection::CompletionResult, String> {
        ReflectionReservationStore::complete(self, run, outcome).map_err(|error| error.to_string())
    }
    fn complete_with_options(
        &self,
        run: &str,
        outcome: memory_core::reflection::ReflectionOutcome,
        options: memory_core::reflection::CompleteOptions,
    ) -> Result<memory_core::reflection::CompletionResult, String> {
        ReflectionReservationStore::complete_with_options(self, run, outcome, options)
            .map_err(|error| error.to_string())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct SweepRecovery;
    impl crate::worker::run_reconciliation::ReflectionRecoveryPort for SweepRecovery {
        fn hostname(&self) -> String {
            "host".into()
        }
        fn pid_liveness(&self, _: u32) -> memory_core::locks::ProcessLiveness {
            memory_core::locks::ProcessLiveness::Dead
        }
        fn process_start(&self, _: u32) -> Option<String> {
            None
        }
        fn classify(
            &self,
            _: Option<u64>,
            _: Option<Option<&str>>,
        ) -> crate::worker::run_liveness::RunProcessVerdict {
            crate::worker::run_liveness::RunProcessVerdict::Absent
        }
        fn wait_outcome<'a>(
            &'a self,
            _: &'a std::path::Path,
            _: f64,
            _: Option<&'a maho_ext_api::AbortSignal>,
        ) -> crate::worker::run_reconciliation::ReconcileFuture<'a> {
            Box::pin(async { Ok(()) })
        }
        fn wait_until<'a>(
            &'a self,
            _: f64,
            _: Option<&'a maho_ext_api::AbortSignal>,
        ) -> crate::worker::run_reconciliation::ReconcileFuture<'a> {
            Box::pin(async { Ok(()) })
        }
        fn signal_group(&self, _: u64, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn fail(
            &self,
            _: &std::path::Path,
            _: &crate::worker::reservation_run_ledger::ReservationRunLedger,
            _: bool,
            _: bool,
        ) -> Result<Option<crate::worker::run_finalization_types::ReservationRunResult>, String>
        {
            Ok(None)
        }
        fn abandon(
            &self,
            _: &std::path::Path,
            _: &crate::worker::reservation_run_ledger::ReservationRunLedger,
        ) -> Result<Option<crate::worker::run_finalization_types::ReservationRunResult>, String>
        {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn the_component_reconcile_reclaims_an_orphan_and_preserves_the_reserved_live_run() {
        let root = tempfile::tempdir().unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        std::fs::create_dir_all(&context.identity_paths.worktrees).unwrap();
        let repo =
            memory_core::git::GitMemoryRepo::open(&context.identity_paths.repo, "agent").unwrap();
        repo.init(Some(memory_core::git::InitializeGitRepoOptions::default()))
            .unwrap();
        let exec = repo.exec();
        let settings = crate::reflection_settings::resolve_memory_settings(None).unwrap();
        let runtime = create_identity_runtime(context.clone(), &settings).unwrap();
        let reserved = runtime
            .store
            .evaluate(
                "session",
                memory_core::reflection::ReflectionEvent::Manual {
                    focus: None,
                    recent_n: None,
                    conversation_ids: None,
                },
            )
            .unwrap()
            .expect("reserved run");
        assert_eq!(reserved.status, "active");
        let live = memory_core::reflection::create_reflection_worktree(
            &repo,
            &reserved.run.run_id,
            &context.identity_paths.worktrees,
            exec.as_ref(),
            None,
        )
        .unwrap();
        let orphan = memory_core::reflection::create_reflection_worktree(
            &repo,
            "run-gone",
            &context.identity_paths.worktrees,
            exec.as_ref(),
            None,
        )
        .unwrap();

        let now = || 4_000_000_000_000_i64;
        let launch = |_run: &memory_core::reflection::ReservedRun| {};
        let results = runtime.reconcile(&now, &SweepRecovery, &launch).await;

        assert_eq!(results, Ok(Vec::new()));
        assert!(!orphan.dir.exists());
        let gone = exec
            .run_in(
                &repo.dir,
                &[
                    "show-ref",
                    "--verify",
                    &format!("refs/heads/{}", orphan.branch),
                ],
            )
            .unwrap();
        assert_ne!(gone.code, 0);
        assert!(live.dir.exists());
        let kept = exec
            .run_in(
                &repo.dir,
                &[
                    "show-ref",
                    "--verify",
                    &format!("refs/heads/{}", live.branch),
                ],
            )
            .unwrap();
        assert_eq!(kept.code, 0);
        let cleanup = memory_core::reflection::discard_reflection_worktree(
            &repo,
            &live.dir,
            &live.branch,
            exec.as_ref(),
        );
        assert!(cleanup.worktree_removed && cleanup.branch_removed);
    }

    #[tokio::test]
    async fn the_component_reconcile_leaves_an_in_grace_orphan_untouched() {
        let root = tempfile::tempdir().unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        std::fs::create_dir_all(&context.identity_paths.worktrees).unwrap();
        let repo =
            memory_core::git::GitMemoryRepo::open(&context.identity_paths.repo, "agent").unwrap();
        repo.init(Some(memory_core::git::InitializeGitRepoOptions::default()))
            .unwrap();
        let exec = repo.exec();
        let young = memory_core::reflection::create_reflection_worktree(
            &repo,
            "run-young",
            &context.identity_paths.worktrees,
            exec.as_ref(),
            None,
        )
        .unwrap();
        let settings = crate::reflection_settings::resolve_memory_settings(None).unwrap();
        let runtime = create_identity_runtime(context.clone(), &settings).unwrap();

        let now = || 0_i64;
        let launch = |_run: &memory_core::reflection::ReservedRun| {};
        let results = runtime.reconcile(&now, &SweepRecovery, &launch).await;

        assert_eq!(results, Ok(Vec::new()));
        assert!(young.dir.exists());
        let cleanup = memory_core::reflection::discard_reflection_worktree(
            &repo,
            &young.dir,
            &young.branch,
            exec.as_ref(),
        );
        assert!(cleanup.worktree_removed && cleanup.branch_removed);
    }
    #[test]
    fn native_store_reserves_manual_run_and_adapts_state() {
        let root = tempfile::tempdir().unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let settings = crate::reflection_settings::resolve_memory_settings(None).unwrap();
        let store = create_identity_reservation_store(&context, &settings).unwrap();
        let result = store
            .evaluate(
                "session",
                memory_core::reflection::ReflectionEvent::Manual {
                    focus: None,
                    recent_n: None,
                    conversation_ids: None,
                },
            )
            .unwrap()
            .unwrap();
        assert_eq!(result.status, "active");
        assert!(result.run.run_id.starts_with("reflection-run-"));
        let state =
            crate::worker::runner_types::ReflectionReservationPort::read_state(store.as_ref())
                .unwrap();
        assert_eq!(state.active.unwrap().run_id, result.run.run_id);
    }
    #[test]
    fn identity_preserves_paths_and_normalizes_slug() {
        let root = tempfile::tempdir().unwrap();
        let context = MemoryIdentityContext::new(
            "Agent One".into(),
            memory_core::identity::layout::build_identity_paths(root.path(), "Agent One"),
            crate::binding::MemorySessionBinding {
                identity: "Agent One".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let identity = as_memory_identity(&context);
        assert_eq!(identity.id, "Agent One");
        assert_eq!(identity.safe_slug, sanitize_to_slug("Agent One"));
        assert_eq!(identity.paths.repo, context.identity_paths.repo);
    }
    #[test]
    fn invalid_trigger_settings_fail_before_store_construction() {
        let root = tempfile::tempdir().unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        assert!(create_identity_reservation_store(&context, &serde_json::Value::Null).is_err());
    }
    #[test]
    fn sandbox_pins_all_agent_directory_environment_aliases() {
        let root = tempfile::tempdir().unwrap();
        let context = MemoryIdentityContext::new(
            "agent".into(),
            memory_core::identity::layout::build_identity_paths(root.path(), "agent"),
            crate::binding::MemorySessionBinding {
                identity: "agent".into(),
                repo_path_hash: "hash".into(),
                bound_at: 0.0,
            },
        );
        let mut sandbox = IdentitySandbox::default();
        let args = crate::worker::spawn_types::ReflectionSpawnArgs {
            memory_root: None,
            max_concurrent_runs: None,
            command: "child".into(),
            args: vec![],
            cwd: root.path().into(),
            env: std::collections::BTreeMap::from([
                ("MAHO_CODING_AGENT_DIR".into(), "/inherited/maho".into()),
                ("PI_CODING_AGENT_DIR".into(), "/inherited/pi".into()),
            ]),
            run_id: Some("run".into()),
            parent_session_file: None,
            attempt: 1,
            hard_deadline_at: 0.0,
            category: "quick".into(),
            conversation_ids: vec![],
            model: "fixture/model".into(),
            thinking: None,
            load_extensions: None,
            next_attempt: None,
            kind: None,
            trigger: None,
            origin: None,
            merge_policy: None,
            target_doc: None,
            system_token_budget: None,
            system_token_target: None,
            worktree: None,
            paths: crate::worker::spawn_types::ReflectionSpawnPaths {
                session_dir: root.path().into(),
                worktree: root.path().into(),
                git_common_dir: root.path().into(),
                transcript: root.path().into(),
                persona: root.path().into(),
                prompt: root.path().into(),
                skills_usage: None,
                memory_usage: None,
                dream_state: None,
                dream_policy: None,
                system_tokens: None,
                audit: None,
                dream_target: None,
            },
        };
        let result = sandbox
            .apply(
                IdentitySandboxInput {
                    identity: &context,
                    policy: crate::sandbox::SandboxPolicy::Off,
                    agent_dir: std::path::Path::new("/granted/agent"),
                    platform: "linux",
                },
                args,
                &|_| None,
                |_| {},
            )
            .unwrap();
        for name in maho_omo_agent_home::resolve_agent_home::AGENT_DIR_ENV_NAMES {
            assert_eq!(
                result.env.get(name).map(String::as_str),
                Some("/granted/agent")
            );
        }
    }

    #[tokio::test]
    async fn an_aborted_observer_stops_before_reconciling_and_a_clean_pass_leaves_no_flight() {
        let root = tempfile::tempdir().unwrap();
        let context = {
            let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
            crate::engine_session::prepare_memory_engine_session(
                "agent",
                &paths,
                Default::default(),
            )
            .unwrap();
            MemoryIdentityContext::new(
                "agent".into(),
                paths,
                crate::binding::MemorySessionBinding {
                    identity: "agent".into(),
                    repo_path_hash: "hash".into(),
                    bound_at: 0.0,
                },
            )
        };
        let runtime = create_identity_runtime(
            context,
            &crate::reflection_settings::resolve_memory_settings(None).unwrap(),
        )
        .unwrap();
        let now = || 4_000_000_000_000_i64;
        let launch = |_: &memory_core::reflection::ReservedRun| {};
        let signal = maho_ext_api::AbortSignal::default();
        signal.abort();
        let aborted = runtime
            .reconcile_with_signal(&now, &SweepRecovery, &launch, Some(&signal))
            .await;
        assert_eq!(
            aborted,
            Err(crate::worker::run_liveness::OBSERVATION_ABORTED.to_owned())
        );
        assert!(runtime.reconciliation.lock().unwrap().is_none());
        assert!(
            runtime
                .reconcile(&now, &SweepRecovery, &launch)
                .await
                .is_ok()
        );
        assert!(runtime.reconciliation.lock().unwrap().is_none());
    }

    /// The recovery port of the overlapping-observer test: the first outcome wait parks until the
    /// test releases it with a scripted result; every later wait returns at once.
    struct GatedRecovery {
        entered: tokio::sync::Notify,
        release: std::sync::Mutex<Option<tokio::sync::oneshot::Receiver<Result<(), String>>>>,
        waits: std::sync::atomic::AtomicUsize,
    }
    impl crate::worker::run_reconciliation::ReflectionRecoveryPort for GatedRecovery {
        fn hostname(&self) -> String {
            "host".into()
        }
        fn pid_liveness(&self, _: u32) -> memory_core::locks::ProcessLiveness {
            memory_core::locks::ProcessLiveness::Alive
        }
        fn process_start(&self, _: u32) -> Option<String> {
            None
        }
        fn classify(
            &self,
            _: Option<u64>,
            _: Option<Option<&str>>,
        ) -> crate::worker::run_liveness::RunProcessVerdict {
            crate::worker::run_liveness::RunProcessVerdict::Alive
        }
        fn wait_outcome<'a>(
            &'a self,
            _: &'a std::path::Path,
            _: f64,
            _: Option<&'a maho_ext_api::AbortSignal>,
        ) -> crate::worker::run_reconciliation::ReconcileFuture<'a> {
            self.waits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let gate = self.release.lock().unwrap().take();
            Box::pin(async move {
                match gate {
                    Some(gate) => {
                        self.entered.notify_one();
                        gate.await.expect("released")
                    }
                    None => Ok(()),
                }
            })
        }
        fn wait_until<'a>(
            &'a self,
            _: f64,
            _: Option<&'a maho_ext_api::AbortSignal>,
        ) -> crate::worker::run_reconciliation::ReconcileFuture<'a> {
            Box::pin(async { Ok(()) })
        }
        fn signal_group(&self, _: u64, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn fail(
            &self,
            _: &std::path::Path,
            _: &crate::worker::reservation_run_ledger::ReservationRunLedger,
            _: bool,
            _: bool,
        ) -> Result<Option<crate::worker::run_finalization_types::ReservationRunResult>, String>
        {
            Ok(None)
        }
        fn abandon(
            &self,
            _: &std::path::Path,
            _: &crate::worker::reservation_run_ledger::ReservationRunLedger,
        ) -> Result<Option<crate::worker::run_finalization_types::ReservationRunResult>, String>
        {
            Ok(None)
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Overlap {
        Manual,
        Different,
        Aborted,
        Shared,
        Success,
        Failure,
    }

    #[tokio::test]
    async fn overlapping_identity_observers_each_own_their_cancellation() {
        use crate::worker::run_liveness::OBSERVATION_ABORTED;
        for kind in [
            Overlap::Manual,
            Overlap::Different,
            Overlap::Aborted,
            Overlap::Shared,
            Overlap::Success,
            Overlap::Failure,
        ] {
            // Given: an identity whose one live run makes the reconcile pass wait on its outcome.
            let root = tempfile::tempdir().unwrap();
            let paths = memory_core::identity::layout::build_identity_paths(root.path(), "agent");
            crate::engine_session::prepare_memory_engine_session(
                "agent",
                &paths,
                Default::default(),
            )
            .unwrap();
            let run = paths.reflection.join("runs/run-1");
            std::fs::create_dir_all(&run).unwrap();
            crate::worker::run_artifacts::write_run_json_atomic(
                &run.join("ledger.json"),
                &serde_json::json!({"version":1,"runId":"run-1","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"missing","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"missing/.git","gitFileSnapshot":"gitdir: missing","commonConfigPath":"config","commonConfigSnapshot":null,"pid":42,"processStart":"supervisor","childPid":43,"childProcessStart":"child"}),
                0o600,
            )
            .unwrap();
            let context = MemoryIdentityContext::new(
                "agent".into(),
                paths,
                crate::binding::MemorySessionBinding {
                    identity: "agent".into(),
                    repo_path_hash: "hash".into(),
                    bound_at: 0.0,
                },
            );
            let runtime = create_identity_runtime(
                context,
                &crate::reflection_settings::resolve_memory_settings(None).unwrap(),
            )
            .unwrap();
            let (release, gate) = tokio::sync::oneshot::channel();
            let recovery = GatedRecovery {
                entered: tokio::sync::Notify::new(),
                release: std::sync::Mutex::new(Some(gate)),
                waits: Default::default(),
            };
            let now = || 0_i64;
            let launch = |_: &memory_core::reflection::ReservedRun| {};
            let controller = maho_ext_api::AbortSignal::default();
            let joiner = maho_ext_api::AbortSignal::default();
            let second_signal = match kind {
                Overlap::Manual => None,
                Overlap::Shared => Some(&controller),
                _ => Some(&joiner),
            };
            // When: the first observer owns the flight and a second joins it before it settles.
            let first = runtime.reconcile_with_signal(&now, &recovery, &launch, Some(&controller));
            let second = async {
                recovery.entered.notified().await;
                let second = runtime.reconcile_with_signal(&now, &recovery, &launch, second_signal);
                tokio::pin!(second);
                tokio::select! {
                    biased;
                    _ = &mut second => panic!("the joiner settled before the flight"),
                    () = std::future::ready(()) => {}
                }
                if matches!(kind, Overlap::Aborted) {
                    joiner.abort();
                }
                if !matches!(kind, Overlap::Success) {
                    controller.abort();
                }
                let scripted = match kind {
                    Overlap::Failure => Err("reservation read failed".to_owned()),
                    _ => Ok(()),
                };
                release.send(scripted).unwrap();
                second.await
            };
            let (first, second) = tokio::join!(first, second);
            // Then
            let aborted = Err(OBSERVATION_ABORTED.to_owned());
            let failure = Err("reservation read failed".to_owned());
            let expected_first = match kind {
                Overlap::Success => Ok(vec![]),
                Overlap::Failure => failure.clone(),
                _ => aborted.clone(),
            };
            let expected_second = match kind {
                Overlap::Shared | Overlap::Aborted => aborted,
                Overlap::Failure => failure,
                _ => Ok(vec![]),
            };
            assert_eq!(first, expected_first, "{kind:?} first");
            assert_eq!(second, expected_second, "{kind:?} second");
            let passes = match kind {
                Overlap::Manual | Overlap::Different => 2,
                _ => 1,
            };
            assert_eq!(
                recovery.waits.load(std::sync::atomic::Ordering::SeqCst),
                passes,
                "{kind:?} passes"
            );
            assert!(
                runtime.reconciliation.lock().unwrap().is_none(),
                "{kind:?} flight"
            );
            assert!(
                runtime.reconcile(&now, &recovery, &launch).await.is_ok(),
                "{kind:?} later pass"
            );
        }
    }
}
