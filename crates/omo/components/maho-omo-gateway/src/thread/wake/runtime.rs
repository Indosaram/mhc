//! Process-level cold-wake runtime & latch (`thread/wake/runtime.ts`).

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};

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
    carrier: Arc<Carrier>,
    serving: Mutex<Option<Arc<ServingConsumer>>>,
    warn: WakeWarnFn,
    on_wake: Option<OnWakeFn>,
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
        if let Some(s) = self
            .serving
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            s.close();
        }
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
    if let Some(existing) = latch.get(&agent_dir) {
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
        carrier: carrier.clone(),
        serving: Mutex::new(None),
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
                *running_bg
                    .serving
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(consumer));
            }
        });
    }

    Some(running)
}
