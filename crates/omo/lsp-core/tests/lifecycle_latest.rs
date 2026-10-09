use lsp_core::abort::AbortSignal;
use lsp_core::lsp::client::LspDiagnosticsResult;
use lsp_core::lsp::constants::CLIENT_RESPAWN_COOLDOWN_MS;
use lsp_core::lsp::errors::LspError;
use lsp_core::lsp::manager::{
    ClientFuture, LspManager, LspManagerOptions, ManagedLspClient, SharedClient,
};
use lsp_core::lsp::types::ResolvedServer;
use pretty_assertions::assert_eq;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::Notify;

fn test_server() -> ResolvedServer {
    ResolvedServer {
        id: "typescript".to_string(),
        command: vec!["typescript-language-server".to_string(), "--stdio".to_string()],
        extensions: vec![".ts".to_string()],
        priority: 1.0,
        env: None,
        initialization: None,
    }
}

fn assert_err_contains<T>(res: Result<T, LspError>, needle: &str) {
    match res {
        Err(err) => assert!(
            err.to_string().contains(needle),
            "expected error containing {:?}, got {:?}",
            needle,
            err
        ),
        Ok(_) => panic!("expected error containing {:?}, got Ok", needle),
    }
}

#[derive(Default)]
struct EventLogState {
    entries: Vec<String>,
    notifiers: HashMap<String, Arc<Notify>>,
}

#[derive(Default)]
struct EventLog {
    state: Mutex<EventLogState>,
}

impl EventLog {
    fn push(&self, event: &str) {
        let mut state = self.state.lock().expect("lifecycle test state lock");
        state.entries.push(event.to_string());
        if let Some(notify) = state.notifiers.get(event) {
            notify.notify_waiters();
        }
    }

    async fn wait_for(&self, event: &str) {
        let notified = {
            let mut state = self.state.lock().expect("lifecycle test state lock");
            if state.entries.iter().any(|e| e == event) {
                return;
            }
            let notify = state
                .notifiers
                .entry(event.to_string())
                .or_insert_with(|| Arc::new(Notify::new()))
                .clone();
            notify.notified_owned()
        };
        tokio::time::timeout(Duration::from_secs(5), notified)
            .await
            .expect("event arrives before deadline");
    }

    fn entries(&self) -> Vec<String> {
        self.state.lock().expect("lifecycle test state lock").entries.clone()
    }
}

#[derive(Default)]
struct StopGateState {
    gates: HashMap<String, Arc<Notify>>,
    released: HashSet<String>,
}

#[derive(Default)]
struct StopGateRegistry {
    state: Mutex<StopGateState>,
}

impl StopGateRegistry {
    fn release(&self, root: &str) {
        let mut state = self.state.lock().expect("lifecycle test state lock");
        state.released.insert(root.to_string());
        if let Some(notify) = state.gates.get(root) {
            notify.notify_one();
        }
    }

    fn release_all(&self) {
        let mut state = self.state.lock().expect("lifecycle test state lock");
        let gates: Vec<_> = state.gates.iter().map(|(root, notify)| (root.clone(), notify.clone())).collect();
        for (root, notify) in gates {
            state.released.insert(root.clone());
            notify.notify_one();
        }
    }

    async fn wait_for_release(&self, root: &str) {
        let notified = {
            let mut state = self.state.lock().expect("lifecycle test state lock");
            if state.released.contains(root) {
                return;
            }
            let notify = state
                .gates
                .entry(root.to_string())
                .or_insert_with(|| Arc::new(Notify::new()))
                .clone();
            notify.notified_owned()
        };
        tokio::time::timeout(Duration::from_secs(5), notified)
            .await
            .expect("stop is explicitly released before deadline");
    }
}

struct RecordingClientOptions {
    gated_stop: bool,
    stop_registry: Arc<StopGateRegistry>,
    initialize_error: bool,
    alive: bool,
}

struct RecordingFakeClient {
    root: String,
    log: Arc<EventLog>,
    options: RecordingClientOptions,
    alive: AtomicBool,
    stop_call_count: AtomicUsize,
}

impl RecordingFakeClient {
    fn new(root: String, log: Arc<EventLog>, options: RecordingClientOptions) -> Self {
        let alive = AtomicBool::new(options.alive);
        Self {
            root,
            log,
            options,
            alive,
            stop_call_count: AtomicUsize::new(0),
        }
    }
}

impl ManagedLspClient for RecordingFakeClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async {
            self.log.push(&format!("spawn:{}", self.root));
            Ok(())
        })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async {
            if self.options.initialize_error {
                return Err(LspError::other("initialize boom"));
            }
            Ok(())
        })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn command(&self) -> Vec<String> {
        vec!["typescript-language-server".to_string(), "--stdio".to_string()]
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(async {
            self.stop_call_count.fetch_add(1, Ordering::SeqCst);
            self.log.push(&format!("stop-start:{}", self.root));
            if self.options.gated_stop {
                self.options.stop_registry.wait_for_release(&self.root).await;
            }
            self.alive.store(false, Ordering::SeqCst);
            self.log.push(&format!("stop-end:{}", self.root));
        })
    }

    fn diagnostics<'a>(
        &'a self,
        _file_path: &'a str,
        _signal: Option<&'a AbortSignal>,
    ) -> ClientFuture<'a, Result<LspDiagnosticsResult, LspError>> {
        Box::pin(async {
            Ok(LspDiagnosticsResult {
                items: vec![],
                transient_error: None,
            })
        })
    }
}

#[derive(Default)]
struct HarnessOptions {
    max_resident_clients: Option<usize>,
    gated_stop: bool,
    initialize_failures: usize,
    alive_from_generation: usize,
}

#[derive(Clone)]
struct Harness {
    manager: Arc<LspManager>,
    clients: Arc<Mutex<Vec<Arc<RecordingFakeClient>>>>,
    log: Arc<EventLog>,
    clock: Arc<Mutex<f64>>,
    stop_registry: Arc<StopGateRegistry>,
}

impl Harness {
    fn release_stop(&self, root: &str) {
        self.stop_registry.release(root);
    }

    async fn stop_all(&self) {
        self.stop_registry.release_all();
        self.manager.stop_all().await;
    }
}

fn manager_harness(options: HarnessOptions) -> Harness {
    let clients: Arc<Mutex<Vec<Arc<RecordingFakeClient>>>> = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::new(EventLog::default());
    let clock = Arc::new(Mutex::new(1000.0));
    let generations: Arc<Mutex<HashMap<String, usize>>> = Arc::new(Mutex::new(HashMap::new()));
    let stop_registry = Arc::new(StopGateRegistry::default());

    let alive_from = if options.alive_from_generation == 0 {
        1
    } else {
        options.alive_from_generation
    };
    let gated_stop = options.gated_stop;
    let initialize_failures = options.initialize_failures;

    let clients_clone = Arc::clone(&clients);
    let log_clone = Arc::clone(&log);
    let clock_clone = Arc::clone(&clock);
    let stop_registry_clone = Arc::clone(&stop_registry);

    let manager = LspManager::new(LspManagerOptions {
        max_resident_clients: options.max_resident_clients.or(Some(6)),
        reaper_interval_ms: Some(60_000),
        idle_timeout_ms: Some(60_000_000),
        init_timeout_ms: Some(60_000),
        now: Some(Arc::new(move || *clock_clone.lock().expect("lifecycle test state lock"))),
        client_factory: Some(Arc::new(move |root: &str, _server: &ResolvedServer| {
            let mut gen_map = generations.lock().expect("lifecycle test state lock");
            let gen_count = gen_map.entry(root.to_string()).or_insert(0);
            *gen_count += 1;
            let current_gen = *gen_count;

            let client = Arc::new(RecordingFakeClient::new(
                root.to_string(),
                Arc::clone(&log_clone),
                RecordingClientOptions {
                    gated_stop,
                    stop_registry: Arc::clone(&stop_registry_clone),
                    alive: current_gen >= alive_from,
                    initialize_error: current_gen <= initialize_failures,
                },
            ));
            clients_clone.lock().expect("lifecycle test state lock").push(Arc::clone(&client));
            Ok(client as SharedClient)
        })),
    });

    Harness {
        manager,
        clients,
        log,
        clock,
        stop_registry,
    }
}

// =========================================================================
// LspManager pending-stop tombstones
// =========================================================================

#[tokio::test]
async fn given_a_slow_stopping_client_evicted_at_the_resident_cap_when_the_evicted_root_is_re_acquired_while_the_stop_is_pending_then_the_replacement_spawns_only_after_the_old_client_stopped() {
    let server = test_server();
    let harness = manager_harness(HarnessOptions {
        max_resident_clients: Some(1),
        gated_stop: true,
        ..Default::default()
    });

    let manager = &harness.manager;
    manager.get_client("/root-a", &server, None).await.unwrap();
    manager.release_client("/root-a", &server.id);
    harness.release_stop("/root-b");

    // when: admitting root-b synchronously evicts idle root-a (spawn stop), and root-a is
    // re-acquired in the same window while the evicted client's stop is still pending.
    let root_b = tokio::spawn({
        let manager = Arc::clone(manager);
        let server = server.clone();
        async move { manager.get_client("/root-b", &server, None).await }
    });
    harness.log.wait_for("stop-start:/root-a").await;
    let root_a = tokio::spawn({
        let manager = Arc::clone(manager);
        let server = server.clone();
        async move { manager.get_client("/root-a", &server, None).await }
    });
    harness.release_stop("/root-a");
    root_b.await.unwrap().unwrap();
    root_a.await.unwrap().unwrap();

    manager.release_client("/root-b", &server.id);
    manager.release_client("/root-a", &server.id);

    // then
    let root_a_events: Vec<String> = harness
        .log
        .entries()
        .into_iter()
        .filter(|e| e.ends_with("/root-a"))
        .collect();
    assert_eq!(
        root_a_events,
        vec![
            "spawn:/root-a",
            "stop-start:/root-a",
            "stop-end:/root-a",
            "spawn:/root-a",
        ]
    );

    harness.stop_all().await;
}

#[tokio::test]
async fn given_an_invalidated_client_whose_stop_is_still_pending_when_the_same_root_is_re_acquired_then_the_replacement_spawns_only_after_the_invalidated_client_stopped() {
    let server = test_server();
    let harness = manager_harness(HarnessOptions {
        gated_stop: true,
        ..Default::default()
    });

    let manager = &harness.manager;
    manager.get_client("/root-a", &server, None).await.unwrap();
    manager.release_client("/root-a", &server.id);

    // when
    let release_task = tokio::spawn({
        let harness = harness.clone();
        async move {
            harness.log.wait_for("stop-start:/root-a").await;
            harness.release_stop("/root-a");
        }
    });

    manager.invalidate_client("/root-a", &server.id, None);
    manager.get_client("/root-a", &server, None).await.unwrap();
    release_task.await.unwrap();

    // then
    assert_eq!(
        harness.log.entries(),
        vec![
            "spawn:/root-a",
            "stop-start:/root-a",
            "stop-end:/root-a",
            "spawn:/root-a",
        ]
    );

    harness.stop_all().await;
}

#[tokio::test]
async fn given_a_client_whose_initialization_failed_while_its_stop_is_pending_when_another_get_client_races_the_stop_then_the_replacement_spawns_only_after_the_failed_client_stopped() {
    let server = test_server();
    let harness = manager_harness(HarnessOptions {
        gated_stop: true,
        initialize_failures: 1,
        ..Default::default()
    });

    let manager = &harness.manager;
    let failing = tokio::spawn({
        let manager = Arc::clone(manager);
        let server = server.clone();
        async move { manager.get_client("/root-fail", &server, None).await }
    });

    // when: the second acquisition starts once the first one's stop is provably in flight.
    harness.log.wait_for("stop-start:/root-fail").await;
    let racing = manager.get_client("/root-fail", &server, None);
    harness.release_stop("/root-fail");

    let failing_res = failing.await.expect("failed acquisition task");
    assert_err_contains(failing_res, "initialize boom");

    let racing_res = racing.await;
    assert!(racing_res.is_ok());

    manager.release_client("/root-fail", &server.id);

    // then
    assert_eq!(
        harness.log.entries(),
        vec![
            "spawn:/root-fail",
            "stop-start:/root-fail",
            "stop-end:/root-fail",
            "spawn:/root-fail",
        ]
    );

    harness.stop_all().await;
}

// =========================================================================
// LspManager dead-client respawn budget
// =========================================================================

#[tokio::test]
async fn given_a_crash_looping_client_that_is_dead_on_every_acquisition_when_get_client_is_called_repeatedly_then_respawn_attempts_stay_within_the_retry_budget() {
    let server = test_server();
    let harness = manager_harness(HarnessOptions {
        alive_from_generation: usize::MAX,
        ..Default::default()
    });

    let manager = &harness.manager;

    // when
    for _ in 0..8 {
        if manager.get_client("/root-crash", &server, None).await.is_ok() {
            manager.release_client("/root-crash", &server.id);
        }
    }

    // then: one initial generation plus at most two budgeted respawns.
    assert!(harness.clients.lock().expect("lifecycle test state lock").len() <= 3);

    harness.stop_all().await;
}

#[tokio::test]
async fn given_the_respawn_budget_is_exhausted_when_get_client_is_called_again_then_it_rejects_with_a_budget_error_instead_of_respawning() {
    let server = test_server();
    let harness = manager_harness(HarnessOptions {
        alive_from_generation: usize::MAX,
        ..Default::default()
    });

    let manager = &harness.manager;

    let first = manager.get_client("/root-crash", &server, None).await;
    assert_err_contains(first, "respawn budget exhausted");

    // when / then
    let second = manager.get_client("/root-crash", &server, None).await;
    assert_err_contains(second, "respawn budget exhausted");

    harness.stop_all().await;
}

#[tokio::test]
async fn given_the_budget_was_exhausted_but_the_cooldown_elapsed_and_the_server_recovered_when_get_client_is_called_then_it_spawns_again_and_resets_the_budget_on_a_healthy_generation() {
    let server = test_server();
    // given: generations one and two crash, the third is healthy.
    let harness = manager_harness(HarnessOptions {
        alive_from_generation: 3,
        ..Default::default()
    });

    let manager = &harness.manager;

    let first = manager.get_client("/root-crash", &server, None).await;
    assert_err_contains(first, "respawn budget exhausted");

    // when: cooldown elapses (60s default) and the server stays alive again.
    *harness.clock.lock().expect("lifecycle test state lock") += (CLIENT_RESPAWN_COOLDOWN_MS + 1) as f64;
    let recovered = manager.get_client("/root-crash", &server, None).await;
    assert!(recovered.is_ok());
    let recovered_client = recovered.unwrap();
    manager.release_client("/root-crash", &server.id);

    // then
    assert_eq!(harness.clients.lock().expect("lifecycle test state lock").len(), 3);
    let next = manager.get_client("/root-crash", &server, None).await;
    assert!(next.is_ok());
    let next_client = next.unwrap();
    assert!(Arc::ptr_eq(&recovered_client, &next_client));
    manager.release_client("/root-crash", &server.id);

    harness.stop_all().await;
}
