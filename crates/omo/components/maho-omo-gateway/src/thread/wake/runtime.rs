//! Process-level cold-wake runtime & latch (`thread/wake/runtime.ts`).

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, TryLockError};

use super::super::intake_admission::IntakeAdmission;
use super::super::same_device_wake::{CarrierWakeFn, SameDeviceWake};
use super::super::store::{GatewayStoreOptions, create_gateway_store};
use super::super::store_api::GatewayStoreApi;
use super::carrier::{Carrier, CarrierEvaluation, CarrierOptions, CarrierWakeOutcome};
use super::carrier_connection::{CarrierHostConnection, UnixCarrierHostConnection};
use super::consumer_start::{ConsumerStartInput, ServingConsumer, start_delivery_consumer};
use super::engine_minimum::{MIN_WAKE_ENGINE_VERSION, engine_drops_retained_record};
use super::host_ensure::{EngineHostEnsureOptions, engine_host_ensure};
use super::memory_headroom::{read_host_memory_headroom, read_wake_memory_floor};

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

pub type WakeConnectFn = super::carrier::CarrierConnectFn;
pub type WakeDeliveriesFn = super::consumer_start::DeliveriesPortFn;
pub type WakeSameDeviceFn = super::carrier::CarrierSameDeviceFn;
pub type OnWakeFn = Arc<dyn Fn(&str, &CarrierWakeOutcome) + Send + Sync>;
pub type WakeWarnFn = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Clone, Default)]
pub struct ThreadWakeOptions {
    pub store: Option<Arc<dyn GatewayStoreApi>>,
    pub connect: Option<WakeConnectFn>,
    pub ensure_host: Option<Arc<dyn Fn() -> BoxFuture<()> + Send + Sync>>,
    pub socket: Option<String>,
    pub key_root: Option<PathBuf>,
    pub deliveries: Option<WakeDeliveriesFn>,
    pub engine_version: Option<String>,
    pub minimum_engine: Option<String>,
    pub same_device_enabled: Option<WakeSameDeviceFn>,
    pub wake_limit: Option<u64>,
    pub idle_exit_ms: Option<u64>,
    pub memory_headroom: Option<Arc<dyn Fn() -> Option<u64> + Send + Sync>>,
    pub on_wake: Option<OnWakeFn>,
    pub warn: Option<WakeWarnFn>,
}

pub struct RunningThreadWake {
    agent_dir: PathBuf,
    carrier: Arc<Carrier>,
    serving: Mutex<Option<Arc<ServingConsumer>>>,
    stopped: AtomicBool,
    warn: WakeWarnFn,
    on_wake: Option<OnWakeFn>,
}

pub(crate) fn take_and_run<T>(slot: &Mutex<Option<T>>, action: impl FnOnce(T)) {
    let value = slot
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(value) = value {
        action(value);
    }
}

impl RunningThreadWake {
    pub async fn wake(&self, durable_id: &str) {
        let outcome = self.carrier.wake(durable_id).await;
        if matches!(outcome, CarrierWakeOutcome::NothingQueued) {
            let serving = self
                .serving
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            if let Some(s) = serving {
                let _ = s.report_drained().await;
            }
        }
        if let CarrierWakeOutcome::Failed { attempts } = outcome {
            (self.warn)(&format!(
                "thread wake: {durable_id} was not started after {attempts} attempts"
            ));
        }
        if let Some(ref cb) = self.on_wake {
            cb(durable_id, &outcome);
        }
    }

    pub async fn admit_intake(&self, device: &str) -> IntakeAdmission {
        let serving = self
            .serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match serving {
            Some(s) => s.admit_intake(device).await,
            None => IntakeAdmission::Unknown,
        }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let latch = match process_latch().try_lock() {
            Ok(latch) => Some(latch),
            Err(TryLockError::Poisoned(error)) => Some(error.into_inner()),
            Err(TryLockError::WouldBlock) => None,
        };
        if let Some(mut latch) = latch
            && latch
                .get(&self.agent_dir)
                .is_some_and(|running| std::ptr::eq(Arc::as_ptr(running), self))
        {
            latch.remove(&self.agent_dir);
        }
        take_and_run(&self.serving, |serving| serving.close());
        self.carrier.stop();
    }
}

static LATCH: OnceLock<Mutex<HashMap<PathBuf, Arc<RunningThreadWake>>>> = OnceLock::new();

fn process_latch() -> &'static Mutex<HashMap<PathBuf, Arc<RunningThreadWake>>> {
    LATCH.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn process_same_device_wake(
    store: Arc<dyn GatewayStoreApi>,
    agent_dir: PathBuf,
    sessions_directory: PathBuf,
    enabled: Arc<dyn Fn(&str) -> bool + Send + Sync>,
) -> SameDeviceWake {
    let running = process_latch()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&agent_dir)
        .filter(|running| !running.stopped.load(Ordering::SeqCst))
        .cloned();
    let carrier_wake: Option<CarrierWakeFn> = running.map(|r| {
        Arc::new(move |id: String| {
            let r = r.clone();
            Box::pin(async move {
                r.wake(&id).await;
            }) as BoxFuture<()>
        }) as CarrierWakeFn
    });
    SameDeviceWake {
        store,
        sessions_directory,
        enabled,
        carrier_wake,
    }
}

pub async fn process_intake_admission(agent_dir: &PathBuf, device: &str) -> IntakeAdmission {
    let running = process_latch()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(agent_dir)
        .filter(|running| !running.stopped.load(Ordering::SeqCst))
        .cloned();
    match running {
        Some(r) => r.admit_intake(device).await,
        None => IntakeAdmission::Unknown,
    }
}

fn is_ci_or_task_child() -> bool {
    std::env::var_os("CI").is_some_and(|v| !v.is_empty())
        || std::env::var("OMO_SENPI_TASK_RPC_CHILD").ok().as_deref() == Some("1")
}

pub fn start_thread_wake_at_session(
    mode: &str,
    agent_dir: PathBuf,
    sessions_directory: PathBuf,
    options: ThreadWakeOptions,
) -> Option<Arc<RunningThreadWake>> {
    if !matches!(mode, "tui" | "rpc") || is_ci_or_task_child() || !agent_dir.is_absolute() {
        return None;
    }
    let minimum = options
        .minimum_engine
        .as_deref()
        .or(MIN_WAKE_ENGINE_VERSION);
    if !engine_drops_retained_record(options.engine_version.as_deref(), minimum) {
        return None;
    }

    let mut latch = process_latch()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(existing) = latch
        .get(&agent_dir)
        .filter(|running| !running.stopped.load(Ordering::SeqCst))
    {
        return Some(existing.clone());
    }

    let warn = options
        .warn
        .clone()
        .unwrap_or_else(|| Arc::new(|_line: &str| {}));
    let store: Arc<dyn GatewayStoreApi> = match &options.store {
        Some(s) => s.clone(),
        None => match create_gateway_store(GatewayStoreOptions::new(agent_dir.to_string_lossy())) {
            Ok(s) => s,
            Err(err) => {
                warn(&format!("thread wake: failed to open store: {err}"));
                return None;
            }
        },
    };

    let socket = options
        .socket
        .clone()
        .unwrap_or_else(|| super::super::live_surface_paths::resolve_thread_socket(&agent_dir));
    let ensure_host = options.ensure_host.clone().unwrap_or_else(|| {
        let mut opts = EngineHostEnsureOptions::new(&socket, agent_dir.clone(), warn.clone());
        opts.idle_exit_ms = options.idle_exit_ms;
        engine_host_ensure(opts)
    });
    let key_root = options.key_root.clone();
    let connect = options.connect.clone().unwrap_or_else(|| {
        let sock = socket.clone();
        let kr = key_root.clone();
        Arc::new(move || {
            let sock = sock.clone();
            let kr = kr.clone();
            Box::pin(async move {
                #[cfg(unix)]
                {
                    let c = UnixCarrierHostConnection::connect(&sock, kr).await?;
                    Ok(Arc::new(c) as Arc<dyn CarrierHostConnection>)
                }
                #[cfg(not(unix))]
                {
                    Err(format!("host_unavailable:{sock}"))
                }
            })
        })
    });

    let same_device = options
        .same_device_enabled
        .clone()
        .unwrap_or_else(|| Arc::new(|_| true));
    let wake_limit = options.wake_limit.unwrap_or(0);
    let serving: Arc<Mutex<Option<Arc<ServingConsumer>>>> = Arc::new(Mutex::new(None));
    let serving_for_remote = serving.clone();
    let serving_for_admit = serving.clone();

    let carrier = Carrier::new(CarrierOptions {
        store: store.clone(),
        sessions_directory: sessions_directory.clone(),
        ensure_host,
        connect,
        wake_limit: Some(Arc::new(move || wake_limit)),
        same_device: Some(same_device.clone()),
        remote_wake: Some(Arc::new(move || {
            serving_for_remote
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_some()
        })),
        admit_intake: Some(Arc::new(move |dev: String| {
            let s = serving_for_admit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            Box::pin(async move {
                match s {
                    Some(consumer) => consumer.admit_intake(&dev).await,
                    None => IntakeAdmission::Unknown,
                }
            })
        })),
        memory_headroom: Some(
            options
                .memory_headroom
                .clone()
                .unwrap_or_else(|| Arc::new(read_host_memory_headroom)),
        ),
        memory_floor: Some(Arc::new(read_wake_memory_floor)),
        on_event: Some(Arc::new(move |ev| {
            let ended = matches!(
                ev.outcome,
                CarrierEvaluation::Released
                    | CarrierEvaluation::HandedOver
                    | CarrierEvaluation::ClosedWaiting
                    | CarrierEvaluation::Held
            );
            if ended {
                let s = serving
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .clone();
                if let Some(consumer) = s {
                    tokio::spawn(async move {
                        let _ = consumer.report_drained().await;
                    });
                }
            }
        })),
    });

    let running = Arc::new(RunningThreadWake {
        agent_dir: agent_dir.clone(),
        carrier: carrier.clone(),
        serving: Mutex::new(None),
        stopped: AtomicBool::new(false),
        warn,
        on_wake: options.on_wake.clone(),
    });
    latch.insert(agent_dir.clone(), running.clone());

    if let Some(deliveries) = options.deliveries {
        let running_bg = running.clone();
        let store_bg = store.clone();
        let agent_bg = agent_dir.clone();
        let sess_bg = sessions_directory.clone();
        let ev_bg = options.engine_version.clone();
        let min_bg = options.minimum_engine.clone();
        let sd_bg = Arc::new(move |cwd: Option<&str>| cwd.map(|c| same_device(c)).unwrap_or(true));
        tokio::spawn(async move {
            let r_wake = running_bg.clone();
            let input = ConsumerStartInput {
                store: store_bg,
                agent_dir: agent_bg,
                sessions_directory: sess_bg,
                deliveries: Some(deliveries),
                engine_version: ev_bg,
                minimum_engine: min_bg,
                wake: Arc::new(move |id| {
                    let rw = r_wake.clone();
                    Box::pin(async move { rw.wake(&id).await })
                }),
                same_device: sd_bg,
            };
            if let Some(consumer) = start_delivery_consumer(&input).await {
                let mut serving = running_bg
                    .serving
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if running_bg.stopped.load(Ordering::SeqCst) {
                    drop(serving);
                    consumer.close();
                } else {
                    *serving = Some(Arc::new(consumer));
                }
            }
        });
    }

    Some(running)
}

#[cfg(test)]
mod callback_lock_tests {
    use super::super::consumer_start::DeliveryServicePort;
    use super::*;

    struct TestDeliveryPort {
        closed: Arc<tokio::sync::Notify>,
    }

    impl DeliveryServicePort for TestDeliveryPort {
        fn peer_admitted(&self, _: &str) -> BoxFuture<bool> {
            Box::pin(async { true })
        }

        fn report_drained(&self) -> BoxFuture<Result<(), String>> {
            Box::pin(async { Ok(()) })
        }

        fn close(&self) {
            self.closed.notify_one();
        }
    }

    #[tokio::test]
    async fn stop_closes_consumer_started_after_shutdown() {
        use tokio::sync::oneshot;
        use tokio::time::{Duration, timeout};

        let tmp = tempfile::tempdir().expect("tempdir");
        let (started_tx, started_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let gate = Mutex::new(Some((started_tx, release_rx)));
        let closed = Arc::new(tokio::sync::Notify::new());
        let port: Arc<dyn DeliveryServicePort> = Arc::new(TestDeliveryPort {
            closed: Arc::clone(&closed),
        });
        let options = ThreadWakeOptions {
            engine_version: Some("2026.11.15-2".into()),
            minimum_engine: Some("2026.11.15-2".into()),
            deliveries: Some(Arc::new(move || {
                let (started, release) = gate.lock().expect("gate").take().expect("one start");
                let port = Arc::clone(&port);
                Box::pin(async move {
                    started.send(()).expect("start observed");
                    release.await.expect("release");
                    Some(port)
                })
            })),
            ..Default::default()
        };
        let running = start_thread_wake_at_session(
            "rpc",
            tmp.path().to_path_buf(),
            tmp.path().join("sessions"),
            options,
        )
        .expect("wake starts");
        timeout(Duration::from_secs(5), started_rx)
            .await
            .expect("consumer start timed out")
            .expect("consumer started");
        let close_observed = closed.notified();
        running.stop();
        release_tx.send(()).expect("release consumer");
        timeout(Duration::from_secs(5), close_observed)
            .await
            .expect("late consumer must be closed");
        assert!(running.serving.lock().expect("serving").is_none());
    }

    #[tokio::test]
    async fn stopped_wake_can_restart_without_stale_stop_removing_replacement() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent_dir = tmp.path().to_path_buf();
        let options = ThreadWakeOptions {
            engine_version: Some("2026.11.15-2".into()),
            ..Default::default()
        };
        let start = || {
            start_thread_wake_at_session(
                "rpc",
                agent_dir.clone(),
                agent_dir.join("sessions"),
                options.clone(),
            )
            .expect("wake starts")
        };
        let first = start();
        first.stop();
        let replacement = start();
        assert!(!Arc::ptr_eq(&first, &replacement), "stopped wake reused");
        first.stop();
        assert!(Arc::ptr_eq(&replacement, &start()), "replacement removed");
        replacement.stop();
    }

    #[tokio::test]
    async fn stop_does_not_wait_for_process_latch() {
        use std::sync::mpsc;
        use std::time::Duration;

        let tmp = tempfile::tempdir().expect("tempdir");
        let options = ThreadWakeOptions {
            engine_version: Some("2026.11.15-2".into()),
            ..Default::default()
        };
        let start = || {
            start_thread_wake_at_session(
                "rpc",
                tmp.path().to_path_buf(),
                tmp.path().join("sessions"),
                options.clone(),
            )
            .expect("wake starts")
        };
        let first = start();
        let held = process_latch().lock().expect("latch");
        let (done_tx, done_rx) = mpsc::channel();
        let worker = Arc::clone(&first);
        let thread = std::thread::spawn(move || {
            worker.stop();
            done_tx.send(()).expect("observer");
        });
        let completed = done_rx.recv_timeout(Duration::from_secs(2));
        drop(held);
        thread.join().expect("stop thread");
        completed.expect("stop waited for latch");
        let replacement = start();
        assert!(!Arc::ptr_eq(&first, &replacement));
        replacement.stop();
    }

    #[test]
    fn take_and_run_releases_slot_before_callback() {
        let slot = Mutex::new(Some(7));
        take_and_run(&slot, |value| {
            assert_eq!(value, 7);
            assert!(slot.try_lock().is_ok(), "callback held the slot mutex");
        });
        assert!(slot.lock().expect("slot").is_none());
    }
}
