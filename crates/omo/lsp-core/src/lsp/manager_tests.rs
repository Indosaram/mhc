use super::*;
use crate::abort::AbortSignal;
use crate::lsp::errors::LspError;
use crate::lsp::types::ResolvedServer;
use pretty_assertions::assert_eq;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

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

struct FakeLspClient {
    alive: AtomicBool,
    stop_call_count: Arc<AtomicUsize>,
    command: Vec<String>,
}

impl FakeLspClient {
    fn new(stop_call_count: Arc<AtomicUsize>) -> Self {
        Self {
            alive: AtomicBool::new(true),
            stop_call_count,
            command: vec!["typescript-language-server".to_string(), "--stdio".to_string()],
        }
    }
}

impl ManagedLspClient for FakeLspClient {
    fn start(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async { Ok(()) })
    }

    fn initialize(&self) -> ClientFuture<'_, Result<(), LspError>> {
        Box::pin(async { Ok(()) })
    }

    fn stop(&self) -> ClientFuture<'_, ()> {
        Box::pin(async {
            self.stop_call_count.fetch_add(1, Ordering::SeqCst);
            self.alive.store(false, Ordering::SeqCst);
        })
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    fn command(&self) -> Vec<String> {
        self.command.clone()
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

struct ManagerWithFakes {
    manager: Arc<LspManager>,
    stop_counts: Arc<Mutex<HashMap<String, Arc<AtomicUsize>>>>,
    clock: Arc<Mutex<f64>>,
}

fn manager_with_fakes(max_resident_clients: usize) -> ManagerWithFakes {
    let stop_counts: Arc<Mutex<HashMap<String, Arc<AtomicUsize>>>> = Arc::new(Mutex::new(HashMap::new()));
    let clock = Arc::new(Mutex::new(1000.0));
    let stop_counts_clone = Arc::clone(&stop_counts);
    let clock_clone = Arc::clone(&clock);

    let manager = LspManager::new(LspManagerOptions {
        max_resident_clients: Some(max_resident_clients),
        reaper_interval_ms: Some(60_000),
        idle_timeout_ms: Some(60_000_000),
        now: Some(Arc::new(move || *clock_clone.lock().unwrap())),
        client_factory: Some(Arc::new(move |root: &str, _server: &ResolvedServer| {
            let count = Arc::new(AtomicUsize::new(0));
            stop_counts_clone.lock().unwrap().insert(root.to_string(), Arc::clone(&count));
            Ok(Arc::new(FakeLspClient::new(count)) as SharedClient)
        })),
        ..Default::default()
    });

    ManagerWithFakes {
        manager,
        stop_counts,
        clock,
    }
}

#[tokio::test]
async fn given_a_cap_of_two_idle_clients_when_a_third_client_is_admitted_then_the_least_recently_used_one_is_stopped_and_evicted() {
    let server = test_server();
    let fakes = manager_with_fakes(2);
    let manager = &fakes.manager;

    manager.get_client("/root-a", &server, None).await.unwrap();
    manager.release_client("/root-a", &server.id);
    *fakes.clock.lock().unwrap() += 10.0;

    manager.get_client("/root-b", &server, None).await.unwrap();
    manager.release_client("/root-b", &server.id);
    *fakes.clock.lock().unwrap() += 10.0;

    // when
    manager.get_client("/root-c", &server, None).await.unwrap();
    manager.release_client("/root-c", &server.id);

    // then
    assert_eq!(manager.client_count(), 2);
    assert_eq!(manager.has_client("/root-a", &server.id), false);
    assert_eq!(manager.has_client("/root-b", &server.id), true);
    assert_eq!(manager.has_client("/root-c", &server.id), true);

    let stop_count_a = fakes
        .stop_counts
        .lock()
        .unwrap()
        .get("/root-a")
        .map(|c| c.load(Ordering::SeqCst))
        .unwrap_or(0);
    assert_eq!(stop_count_a, 1);

    manager.stop_all().await;
}

#[tokio::test]
async fn given_the_least_recently_used_client_is_mid_request_when_a_new_client_is_admitted_then_the_busy_client_survives_and_an_idle_one_is_evicted() {
    let server = test_server();
    let fakes = manager_with_fakes(2);
    let manager = &fakes.manager;

    manager.get_client("/root-busy", &server, None).await.unwrap();
    *fakes.clock.lock().unwrap() += 10.0;

    manager.get_client("/root-idle", &server, None).await.unwrap();
    manager.release_client("/root-idle", &server.id);
    *fakes.clock.lock().unwrap() += 10.0;

    // when
    manager.get_client("/root-new", &server, None).await.unwrap();
    manager.release_client("/root-new", &server.id);

    // then
    assert_eq!(manager.has_client("/root-busy", &server.id), true);
    assert_eq!(manager.has_client("/root-idle", &server.id), false);

    let stop_count_busy = fakes
        .stop_counts
        .lock()
        .unwrap()
        .get("/root-busy")
        .map(|c| c.load(Ordering::SeqCst))
        .unwrap_or(0);
    let stop_count_idle = fakes
        .stop_counts
        .lock()
        .unwrap()
        .get("/root-idle")
        .map(|c| c.load(Ordering::SeqCst))
        .unwrap_or(0);

    assert_eq!(stop_count_busy, 0);
    assert_eq!(stop_count_idle, 1);

    manager.release_client("/root-busy", &server.id);
    manager.stop_all().await;
}

#[tokio::test]
async fn given_every_resident_client_is_busy_when_a_new_client_is_admitted_then_nothing_is_evicted_and_the_cap_is_exceeded_rather_than_breaking_a_live_request() {
    let server = test_server();
    let fakes = manager_with_fakes(1);
    let manager = &fakes.manager;

    manager.get_client("/root-busy", &server, None).await.unwrap();

    // when
    manager.get_client("/root-second", &server, None).await.unwrap();

    // then
    assert_eq!(manager.client_count(), 2);
    let stop_count_busy = fakes
        .stop_counts
        .lock()
        .unwrap()
        .get("/root-busy")
        .map(|c| c.load(Ordering::SeqCst))
        .unwrap_or(0);
    assert_eq!(stop_count_busy, 0);

    manager.release_client("/root-busy", &server.id);
    manager.release_client("/root-second", &server.id);
    manager.stop_all().await;
}

#[tokio::test]
async fn given_warmup_requests_beyond_the_cap_when_a_warm_client_is_admitted_then_the_idle_least_recently_used_client_is_evicted_too() {
    let server = test_server();
    let fakes = manager_with_fakes(1);
    let manager = &fakes.manager;

    manager.get_client("/root-first", &server, None).await.unwrap();
    manager.release_client("/root-first", &server.id);
    *fakes.clock.lock().unwrap() += 10.0;

    // when
    manager.warmup_client("/root-warm", &server);
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;

    // then
    assert_eq!(manager.has_client("/root-first", &server.id), false);
    let stop_count_first = fakes
        .stop_counts
        .lock()
        .unwrap()
        .get("/root-first")
        .map(|c| c.load(Ordering::SeqCst))
        .unwrap_or(0);
    assert_eq!(stop_count_first, 1);
    assert_eq!(manager.has_client("/root-warm", &server.id), true);

    manager.stop_all().await;
}
