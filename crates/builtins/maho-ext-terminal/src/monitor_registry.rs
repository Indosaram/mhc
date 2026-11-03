#[derive(Clone, Debug, Default, PartialEq)]
pub struct MonitorSnapshotEntry {
    pub id: String,
    pub monitor_id: Option<String>,
    pub description: String,
    pub paused: bool,
    pub started_at_ms: f64,
    pub command: Option<String>,
    pub filter: Option<String>,
    pub persistent: Option<bool>,
    pub deadline_ms: Option<f64>,
    pub fire_count: Option<usize>,
    pub last_fired_at_ms: Option<f64>,
    pub expires_at: Option<f64>,
    pub fire_window: Option<MonitorFireWindow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MonitorFireWindow {
    pub start_ms: f64,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorEvent {
    Line {
        id: String,
        description: String,
        line: String,
        /// Output lines are a person's words (a chat bridge); goal continuation counts them as new
        /// input. Only a command watch registered with `human_input: true` sets it.
        human_input: bool,
    },
    Summary {
        id: String,
        description: String,
        summary: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MonitorEndedReason {
    Exit,
    Timeout,
    Killed,
    Disposed,
}
impl MonitorEndedReason {
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Timeout => "timeout",
            Self::Killed => "killed",
            Self::Disposed => "disposed",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct MonitorEndedEvent {
    pub id: String,
    /// The stable `mon_` id (upstream `manager.monitorIdOf(event.id)`), when the watch has one.
    pub monitor_id: Option<String>,
    pub description: String,
    pub started_at_ms: f64,
    pub ended_at_ms: f64,
    pub reason: MonitorEndedReason,
    pub exit_code: Option<i32>,
    pub fire_count: usize,
}
type MonitorEndedSink = std::sync::Arc<dyn Fn(MonitorEndedEvent) + Send + Sync>;
fn emit_ended(
    sink: &MonitorEndedSink,
    snapshot: &MonitorSnapshotEntry,
    reason: MonitorEndedReason,
    exit_code: Option<i32>,
    now: f64,
) {
    sink(MonitorEndedEvent {
        id: snapshot.id.clone(),
        monitor_id: snapshot.monitor_id.clone(),
        description: snapshot.description.clone(),
        started_at_ms: snapshot.started_at_ms,
        ended_at_ms: now,
        reason,
        exit_code,
        fire_count: snapshot.fire_count.unwrap_or(0),
    });
}
fn file_settle_reason(summary: &str) -> MonitorEndedReason {
    match summary {
        "watcher timed_out" | "watcher expired" => MonitorEndedReason::Timeout,
        "watcher killed" => MonitorEndedReason::Killed,
        "watcher disposed" => MonitorEndedReason::Disposed,
        _ => MonitorEndedReason::Exit,
    }
}

pub fn allocate_monitor_id() -> std::io::Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 10];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .is_err()
    {
        use sha2::Digest;
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let time = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let pid = std::process::id();
        let mut hasher = sha2::Sha256::new();
        hasher.update(time.to_le_bytes());
        hasher.update(pid.to_le_bytes());
        hasher.update(count.to_le_bytes());
        let hash = hasher.finalize();
        bytes.copy_from_slice(&hash[..10]);
    }
    Ok(monitor_id_from_bytes(bytes))
}
fn monitor_id_from_bytes(bytes: [u8; 10]) -> String {
    let mut id = String::from("mon_");
    let mut buffer = 0u32;
    let mut bits = 0;
    for byte in bytes {
        buffer = (buffer << 8) | u32::from(byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            id.push(b"0123456789ABCDEFGHJKMNPQRSTVWXYZ"[((buffer >> bits) & 31) as usize] as char);
        }
    }
    id
}

/// Upstream monitor-registry.ts re-exports the command source's record types.
pub use crate::command_monitor_source::{CommandMonitor, RunawayWindow};

pub struct MonitorRegistry {
    records: std::sync::Arc<std::sync::Mutex<indexmap::IndexMap<String, CommandMonitor>>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    emit: std::sync::Arc<dyn Fn(MonitorEvent) + Send + Sync>,
    files: indexmap::IndexMap<
        String,
        (
            std::sync::Arc<std::sync::Mutex<crate::file_monitor::FileMonitor>>,
            tokio::task::JoinHandle<()>,
        ),
    >,
    file_snapshots:
        std::sync::Arc<std::sync::Mutex<indexmap::IndexMap<String, MonitorSnapshotEntry>>>,
    rpc_slot: RpcSourceSlot,
    next_file_id: usize,
    transitions: tokio::sync::watch::Sender<Vec<MonitorSnapshotEntry>>,
    parked: tokio::sync::watch::Sender<bool>,
    delivery: std::sync::Arc<std::sync::Mutex<MonitorRouting>>,
    ended: MonitorEndedSink,
    runaway_guard: std::sync::Arc<std::sync::Mutex<crate::settings::MonitorRunawayGuard>>,
    /// Upstream `#rpc`: rpc watches (eager here; upstream loads the source on the first one).
    rpc: crate::rpc_monitor_source::RpcMonitorSource,
    /// File registrations still proving their target (upstream `FileMonitorSource.#guard`).
    registrations: std::sync::Arc<crate::file_monitor_guard::RegistrationGuard>,
    disposed: bool,
    /// Test seam: raises a native watcher error on a live file watch.
    #[cfg(test)]
    watcher_errors: indexmap::IndexMap<String, crate::file_monitor_source::WatcherErrorInjector>,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WakePacing {
    pub wakes_per_hour: Option<f64>,
}
type MonitorSink = std::sync::Arc<dyn Fn(MonitorEvent) + Send + Sync>;
#[derive(Default)]
struct MonitorRouting {
    sink: Option<MonitorSink>,
    pending: std::collections::VecDeque<MonitorEvent>,
}
type RpcSourceSlot =
    std::sync::Arc<std::sync::OnceLock<crate::rpc_monitor_source::RpcMonitorSource>>;
fn publish_snapshot(
    records: &std::sync::Mutex<indexmap::IndexMap<String, CommandMonitor>>,
    files: &std::sync::Mutex<indexmap::IndexMap<String, MonitorSnapshotEntry>>,
    rpc: &RpcSourceSlot,
    sender: &tokio::sync::watch::Sender<Vec<MonitorSnapshotEntry>>,
) {
    let mut snapshot = records
        .lock()
        .expect("monitor records")
        .values()
        .map(|record| record.snapshot.clone())
        .collect::<Vec<_>>();
    snapshot.extend(files.lock().expect("file snapshots").values().cloned());
    if let Some(rpc) = rpc.get() {
        snapshot.extend(rpc.snapshots());
    }
    sender.send_if_modified(|current| {
        if *current == snapshot {
            false
        } else {
            *current = snapshot;
            true
        }
    });
}
fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("epoch")
        .as_secs_f64()
        * 1000.0
}
impl MonitorRegistry {
    pub fn new(emit: impl Fn(MonitorEvent) + Send + Sync + 'static) -> Self {
        Self::new_with_ended(emit, |_| {})
    }
    pub fn new_with_ended(
        emit: impl Fn(MonitorEvent) + Send + Sync + 'static,
        ended: impl Fn(MonitorEndedEvent) + Send + Sync + 'static,
    ) -> Self {
        let delivery = std::sync::Arc::new(std::sync::Mutex::new(MonitorRouting {
            sink: Some(std::sync::Arc::new(emit)),
            ..Default::default()
        }));
        let routing = delivery.clone();
        let emit = std::sync::Arc::new(move |event: MonitorEvent| {
            let sink = {
                let mut routing = routing.lock().expect("monitor routing");
                if let Some(sink) = &routing.sink {
                    Some(sink.clone())
                } else {
                    routing.pending.push_back(event.clone());
                    if routing.pending.len() > 100 {
                        routing.pending.pop_front();
                    }
                    None
                }
            };
            if let Some(sink) = sink {
                sink(event);
            }
        });
        let records: std::sync::Arc<std::sync::Mutex<indexmap::IndexMap<String, CommandMonitor>>> =
            Default::default();
        let file_snapshots: std::sync::Arc<
            std::sync::Mutex<indexmap::IndexMap<String, MonitorSnapshotEntry>>,
        > = Default::default();
        let transitions = tokio::sync::watch::channel(vec![]).0;
        let ended: MonitorEndedSink = std::sync::Arc::new(ended);
        let rpc_slot: RpcSourceSlot = Default::default();
        let rpc = crate::rpc_monitor_source::RpcMonitorSource::new(
            crate::rpc_monitor_source::RpcSourceHost {
                emit: emit.clone(),
                ended: ended.clone(),
                changed: {
                    let (records, files, slot, transitions) = (
                        records.clone(),
                        file_snapshots.clone(),
                        rpc_slot.clone(),
                        transitions.clone(),
                    );
                    std::sync::Arc::new(move || {
                        publish_snapshot(&records, &files, &slot, &transitions)
                    })
                },
                now_ms: std::sync::Arc::new(now_ms),
            },
        );
        let _ = rpc_slot.set(rpc.clone());
        Self {
            records,
            tasks: vec![],
            emit,
            files: Default::default(),
            file_snapshots,
            rpc_slot,
            next_file_id: 0,
            transitions,
            parked: tokio::sync::watch::channel(false).0,
            delivery,
            ended,
            runaway_guard: std::sync::Arc::new(std::sync::Mutex::new(
                crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor_runaway,
            )),
            rpc,
            registrations: Default::default(),
            disposed: false,
            #[cfg(test)]
            watcher_errors: Default::default(),
        }
    }
    pub fn detach_delivery(&self) {
        self.delivery.lock().expect("monitor routing").sink = None;
    }
    pub fn bind_delivery(&self, emit: impl Fn(MonitorEvent) + Send + Sync + 'static) {
        let sink: MonitorSink = std::sync::Arc::new(emit);
        let pending = {
            let mut routing = self.delivery.lock().expect("monitor routing");
            routing.sink = Some(sink.clone());
            std::mem::take(&mut routing.pending)
        };
        for event in pending {
            sink(event);
        }
    }
    /// The current runaway-guard thresholds (settings can change across a reload).
    pub fn runaway_guard(&self) -> crate::settings::MonitorRunawayGuard {
        *self.runaway_guard.lock().expect("runaway guard")
    }
    /// Apply the resolved runaway-guard settings; the registry outlives a reload, the settings do not.
    pub fn set_runaway_guard(&self, guard: crate::settings::MonitorRunawayGuard) {
        *self.runaway_guard.lock().expect("runaway guard") = guard;
    }
    /// Wake pacing of a live watch: only persistent command watches are paced. Returns the watch's own
    /// wake-rate override (`None` = the setting) or `None` when the watch is not paced.
    pub fn wake_pacing(&self, id: &str) -> Option<WakePacing> {
        let records = self.records.lock().expect("monitor records");
        let record = records
            .get(id)
            .filter(|record| record.snapshot.persistent == Some(true))?;
        Some(WakePacing {
            wakes_per_hour: record.wakes_per_hour,
        })
    }
    pub fn park(&self) {
        self.parked.send_replace(true);
    }
    pub fn unpark(&self) {
        self.parked.send_replace(false);
    }
    pub fn subscribe_state(&self) -> tokio::sync::watch::Receiver<Vec<MonitorSnapshotEntry>> {
        self.transitions.subscribe()
    }
    pub fn muted_dropped(&self, id: &str) -> usize {
        self.records
            .lock()
            .expect("monitor records")
            .get(id)
            .map(|record| record.muted_dropped)
            .or_else(|| self.rpc.muted_dropped(id))
            .unwrap_or(0)
    }
    /// The rpc source, for an arm that is awaited without holding the registry lock (upstream
    /// `registerRpc`).
    pub fn rpc_source(&self) -> crate::rpc_monitor_source::RpcMonitorSource {
        self.rpc.clone()
    }
    /// Upstream `stopRpc`: false when the id is not a live rpc watch.
    pub fn stop_rpc(&self, id: &str) -> bool {
        self.rpc.settle(id, "watcher killed")
    }
    /// Upstream `stopAllRpc`.
    pub fn stop_all_rpc(&self) -> usize {
        self.rpc.settle_all("watcher killed")
    }
    fn publish_state(&self) {
        publish_snapshot(
            &self.records,
            &self.file_snapshots,
            &self.rpc_slot,
            &self.transitions,
        );
    }
    pub fn register_file(
        &mut self,
        description: &str,
        path: &std::path::Path,
        event: crate::terminal_manifest_model::FileEvent,
        timeout_ms: u64,
    ) -> std::io::Result<(String, String)> {
        self.register_file_with_identity(description, path, event, timeout_ms, None, None)
    }
    pub fn register_file_with_identity(
        &mut self,
        description: &str,
        path: &std::path::Path,
        event: crate::terminal_manifest_model::FileEvent,
        timeout_ms: u64,
        monitor_id: Option<&str>,
        approved_parent: Option<&std::path::Path>,
    ) -> std::io::Result<(String, String)> {
        self.register_file_lifetime(
            description,
            path,
            event,
            (timeout_ms, false),
            monitor_id,
            approved_parent,
        )
    }
    pub fn register_persistent_file(
        &mut self,
        description: &str,
        path: &std::path::Path,
        event: crate::terminal_manifest_model::FileEvent,
    ) -> std::io::Result<(String, String)> {
        self.register_persistent_file_with_identity(description, path, event, None)
    }
    pub fn register_persistent_file_with_identity(
        &mut self,
        description: &str,
        path: &std::path::Path,
        event: crate::terminal_manifest_model::FileEvent,
        approved_parent: Option<&std::path::Path>,
    ) -> std::io::Result<(String, String)> {
        self.register_file_lifetime(
            description,
            path,
            event,
            (crate::shared::DURABLE_MONITOR_EXPIRY_MS, true),
            None,
            approved_parent,
        )
    }
    pub fn restore_persistent_file(
        &mut self,
        monitor: &crate::terminal_manifest_model::ManifestMonitor,
        path: &std::path::Path,
        now: f64,
    ) -> std::io::Result<(String, String)> {
        self.restore_file_with_lifetime(monitor, path, now, None)
    }
    /// `restore_persistent_file`, or (with `ephemeral_time_left`) an ephemeral watch resumed with
    /// only the time it had left and a live deadline (upstream durable-file.ts `lifetime`).
    pub fn restore_file_with_lifetime(
        &mut self,
        monitor: &crate::terminal_manifest_model::ManifestMonitor,
        path: &std::path::Path,
        now: f64,
        ephemeral_time_left: Option<f64>,
    ) -> std::io::Result<(String, String)> {
        let lifetime = match ephemeral_time_left {
            Some(time_left) => (time_left.max(1.0) as u64, false),
            None => (crate::durable_file::remaining_ms(monitor, now) as u64, true),
        };
        let registered = self.register_file_lifetime(
            &monitor.description,
            path,
            monitor
                .event
                .unwrap_or(crate::terminal_manifest_model::FileEvent::Create),
            lifetime,
            Some(&monitor.monitor_id),
            monitor.approved_parent.as_deref().map(std::path::Path::new),
        )?;
        if let Some(snapshot) = self
            .file_snapshots
            .lock()
            .expect("file snapshots")
            .get_mut(&registered.0)
        {
            snapshot.expires_at = monitor.expires_at;
        }
        self.publish_state();
        Ok(registered)
    }
    fn register_file_lifetime(
        &mut self,
        description: &str,
        path: &std::path::Path,
        event: crate::terminal_manifest_model::FileEvent,
        lifetime: (u64, bool),
        monitor_id: Option<&str>,
        approved_parent: Option<&std::path::Path>,
    ) -> std::io::Result<(String, String)> {
        if self.disposed {
            return Err(std::io::Error::other(
                "Cannot create file monitor: monitor registry is disposed.",
            ));
        }
        let record = crate::file_monitor::FileMonitor::register(
            String::new(),
            description.to_owned(),
            path,
            event,
            approved_parent,
        )?;
        let watcher = crate::file_monitor_source::ParentWatcher::watch(
            &record.parent,
            record.path.file_name().unwrap_or_default(),
        )
        .map_err(|error| {
            std::io::Error::other(format!(
                "Cannot watch file {}: {error}",
                record.path.display()
            ))
        })?;
        self.adopt_file(
            crate::file_monitor_source::ProvenFileWatch { record, watcher },
            lifetime,
            monitor_id,
            None,
        )
    }
    /// Opens a guarded file registration (upstream `registerFile` up to `proveWatchTarget`): the
    /// guard and the pending entry the caller proves the target against, outside any registry lock,
    /// with [`crate::file_monitor_source::prove_file_registration`]. `release` is the capacity slot
    /// the watch will own; a dispose or [`Self::stop_all_files`] aborts the registration and frees it.
    pub fn begin_file_registration(
        &self,
        release: Option<crate::manager::CapacityReservation>,
        timeout_ms: u64,
    ) -> std::io::Result<(
        std::sync::Arc<crate::file_monitor_guard::RegistrationGuard>,
        crate::file_monitor_guard::PendingFileRegistration,
    )> {
        if self.disposed {
            return Err(std::io::Error::other(
                "Cannot create file monitor: monitor registry is disposed.",
            ));
        }
        Ok((
            self.registrations.clone(),
            self.registrations.open(release, timeout_ms),
        ))
    }
    /// Completes a guarded registration: a proof that is still live becomes a watch that owns the
    /// pending capacity slot; a proof that lost the race (deadline, dispose, `stopAllFiles`) or a
    /// disposed registry fails, and the slot is released.
    pub fn finish_file_registration(
        &mut self,
        pending: &crate::file_monitor_guard::PendingFileRegistration,
        proven: std::io::Result<crate::file_monitor_source::ProvenFileWatch>,
        lifetime: (u64, bool),
        monitor_id: Option<&str>,
    ) -> std::io::Result<(String, String)> {
        let proven = match proven {
            Ok(proven) if !pending.is_aborted() && !self.disposed => proven,
            Ok(_) => {
                self.registrations.finish(pending, false);
                return Err(std::io::Error::other(if self.disposed {
                    "Cannot create file monitor: monitor registry is disposed."
                } else {
                    crate::file_monitor_guard::REGISTRATION_ABORTED
                }));
            }
            Err(error) => {
                self.registrations.finish(pending, false);
                return Err(error);
            }
        };
        let reservation = self.registrations.finish(pending, true);
        self.adopt_file(proven, lifetime, monitor_id, reservation)
    }
    /// `registerFile` end to end: reserve, prove under the registration deadline without holding
    /// the registry lock across a filesystem step, then adopt the watch.
    pub async fn register_file_guarded(
        registry: &std::sync::Mutex<Self>,
        request: crate::file_monitor_source::FileRegistrationRequest,
        release: Option<crate::manager::CapacityReservation>,
        timeout_ms: u64,
        lifetime: (u64, bool),
        monitor_id: Option<&str>,
    ) -> std::io::Result<(String, String)> {
        let (guard, pending) = registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin_file_registration(release, timeout_ms)?;
        let proven =
            crate::file_monitor_source::prove_file_registration(&guard, &pending, request).await;
        registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish_file_registration(&pending, proven, lifetime, monitor_id)
    }
    fn adopt_file(
        &mut self,
        proven: crate::file_monitor_source::ProvenFileWatch,
        lifetime: (u64, bool),
        monitor_id: Option<&str>,
        reservation: Option<crate::manager::CapacityReservation>,
    ) -> std::io::Result<(String, String)> {
        let crate::file_monitor_source::ProvenFileWatch {
            mut record,
            watcher,
        } = proven;
        let (timeout_ms, persistent) = lifetime;
        let id = format!("watch_{}", self.next_file_id + 1);
        let monitor_id = match monitor_id {
            Some(id) => id.to_owned(),
            None => allocate_monitor_id()?,
        };
        let description = record.description.clone();
        let description = description.as_str();
        record.id = id.clone();
        record.reservation = reservation;
        let file = std::sync::Arc::new(std::sync::Mutex::new(record));
        let checker = file.clone();
        let emit = self.emit.clone();
        let snapshots = self.file_snapshots.clone();
        let runtime_id = id.clone();
        let ended = self.ended.clone();
        snapshots.lock().expect("file snapshots").insert(
            id.clone(),
            MonitorSnapshotEntry {
                id: id.clone(),
                monitor_id: Some(monitor_id.clone()),
                description: description.to_owned(),
                started_at_ms: now_ms(),
                persistent: Some(persistent),
                deadline_ms: (!persistent).then_some(now_ms() + timeout_ms as f64),
                expires_at: persistent.then_some(now_ms() + timeout_ms as f64),
                ..Default::default()
            },
        );
        snapshots
            .lock()
            .expect("file snapshots")
            .get_mut(&id)
            .expect("registered file snapshot")
            .fire_count = Some(0);
        self.publish_state();
        let records = self.records.clone();
        let rpc_slot = self.rpc_slot.clone();
        let transitions = self.transitions.clone();
        let mut state = self.subscribe_state();
        let mut parked = self.parked.subscribe();
        let expires = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        let wake = watcher.wake();
        #[cfg(test)]
        self.watcher_errors.insert(id.clone(), watcher.error_injector());
        let watch = tokio::spawn(async move {
            let period =
                std::time::Duration::from_millis(crate::monitor_file_watch::FILE_MONITOR_POLL_MS);
            let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let deadline = tokio::time::sleep_until(expires);
            tokio::pin!(deadline);
            loop {
                let paused = checker.lock().expect("file monitor").paused || *parked.borrow();
                let timed_out = tokio::select! {
                    _=timer.tick(),if !paused=>false,
                    // A watcher error settles even a paused or parked watch (upstream settles on
                    // the watcher's `error` event at once), so the wake is always listened to.
                    _=wake.notified()=>false,
                    _=&mut deadline=>true,
                    changed=state.changed()=>{if changed.is_err() {return;}state.borrow_and_update();timer.reset();continue;}
                    changed=parked.changed()=>{if changed.is_err() {return;}let resumed=!*parked.borrow_and_update();if resumed {timer.reset_immediately();}else {timer.reset();}continue;}
                };
                // The poll does blocking lstat/open/digest work, so it runs off the async workers.
                let blocking = checker.clone();
                let watcher_error = watcher.take_error();
                // A plain change while paused waits for the resume, which checks at once.
                if paused && !timed_out && watcher_error.is_none() {
                    continue;
                }
                let Ok((events, settled, summary)) = tokio::task::spawn_blocking(move || {
                    let mut summary = None;
                    let mut file = blocking.lock().expect("file monitor");
                    let events: Vec<MonitorEvent> = if let Some(error) = watcher_error {
                        let events: Vec<MonitorEvent> = file.stop(&error).into_iter().collect();
                        summary = Some(error);
                        events
                    } else if timed_out {
                        summary = Some(
                            (if persistent {
                                "watcher expired"
                            } else {
                                "watcher timed_out"
                            })
                            .to_owned(),
                        );
                        file.stop(summary.as_deref().unwrap_or("watcher timed_out"))
                            .into_iter()
                            .collect()
                    } else {
                        match file.check() {
                            Ok(events) => events,
                            Err(error) => {
                                summary = Some(format!("watcher error: {error}"));
                                file.stop(summary.as_deref().unwrap_or("watcher error"))
                                    .into_iter()
                                    .collect()
                            }
                        }
                    };
                    (events, file.settled, summary)
                })
                .await
                else {
                    return;
                };
                if settled {
                    let snapshot = snapshots
                        .lock()
                        .expect("file snapshots")
                        .shift_remove(&runtime_id);
                    if let Some(snapshot) = snapshot {
                        emit_ended(
                            &ended,
                            &snapshot,
                            file_settle_reason(summary.as_deref().unwrap_or("")),
                            None,
                            now_ms(),
                        );
                    }
                }
                publish_snapshot(&records, &snapshots, &rpc_slot, &transitions);
                for event in events {
                    emit(event);
                }
                if settled {
                    return;
                }
            }
        });
        self.files.retain(|_, (_, task)| !task.is_finished());
        self.next_file_id += 1;
        self.files.insert(id.clone(), (file, watch));
        Ok((id, monitor_id))
    }
    /// `stopAllFiles`: kill every live file watch and abort every registration still proving its
    /// target (its capacity is released). Returns how many live watches were stopped.
    pub fn stop_all_files(&mut self) -> usize {
        let ids = self
            .file_snapshots
            .lock()
            .expect("file snapshots")
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        self.registrations.finish_all();
        ids.into_iter().filter(|id| self.stop_file(id)).count()
    }
    pub fn stop_file(&mut self, id: &str) -> bool {
        let Some((file, watch)) = self.files.shift_remove(id) else {
            return false;
        };
        watch.abort();
        let snapshot = self
            .file_snapshots
            .lock()
            .expect("file snapshots")
            .shift_remove(id);
        let event = file.lock().expect("file monitor").stop("watcher killed");
        self.publish_state();
        if let Some(event) = event {
            (self.emit)(event);
        }
        if let Some(snapshot) = snapshot {
            emit_ended(
                &self.ended,
                &snapshot,
                MonitorEndedReason::Killed,
                None,
                now_ms(),
            );
        }
        true
    }
    pub fn file_checkpoint(
        &self,
        id: &str,
    ) -> Option<crate::terminal_manifest_model::TerminalManifestCheckpoint> {
        let (file, _) = self.files.get(id)?;
        let file = file.lock().expect("file monitor");
        if file.settled {
            None
        } else {
            Some(file.checkpoint.clone())
        }
    }
    pub fn reserve_file_capacity(
        &self,
        id: &str,
        reservation: crate::manager::CapacityReservation,
    ) {
        if let Some((file, _)) = self.files.get(id) {
            file.lock()
                .expect("file monitor")
                .reserve_capacity(reservation);
        }
    }
    pub fn adopt_fire_window(
        &self,
        monitor_id: &str,
        window: &crate::terminal_manifest_model::ManifestFireWindow,
    ) {
        let window = MonitorFireWindow {
            start_ms: window.start_ms,
            count: window.count as usize,
        };
        for record in self.records.lock().expect("monitor records").values_mut() {
            if record.snapshot.monitor_id.as_deref() == Some(monitor_id) {
                record.snapshot.fire_window = Some(window.clone());
            }
        }
        for record in self
            .file_snapshots
            .lock()
            .expect("file snapshots")
            .values_mut()
        {
            if record.monitor_id.as_deref() == Some(monitor_id) {
                record.fire_window = Some(window.clone());
            }
        }
        self.publish_state();
    }
    pub fn emit_file_line(&self, id: &str, line: String) -> bool {
        let Some((file, _)) = self.files.get(id) else {
            return false;
        };
        let event = {
            let file = file.lock().expect("file monitor");
            if file.settled {
                return false;
            }
            MonitorEvent::Line {
                id: file.id.clone(),
                description: file.description.clone(),
                line,
                human_input: false,
            }
        };
        if let Some(snapshot) = self
            .file_snapshots
            .lock()
            .expect("file snapshots")
            .get_mut(id)
        {
            snapshot.fire_count = Some(snapshot.fire_count.unwrap_or(0) + 1);
            snapshot.last_fired_at_ms = Some(now_ms());
        }
        self.publish_state();
        (self.emit)(event);
        true
    }
    pub fn snapshot(&self) -> Vec<MonitorSnapshotEntry> {
        let mut snapshot = self
            .records
            .lock()
            .expect("monitor records")
            .values()
            .map(|record| record.snapshot.clone())
            .collect::<Vec<_>>();
        snapshot.extend(
            self.file_snapshots
                .lock()
                .expect("file snapshots")
                .values()
                .cloned(),
        );
        snapshot.extend(self.rpc.snapshots());
        snapshot
    }
    pub fn pause(&self, ids: &[String]) -> Vec<String> {
        let mut records = self.records.lock().expect("monitor records");
        let mut paused = ids
            .iter()
            .filter(|id| records.get_mut(*id).is_some_and(CommandMonitor::pause))
            .cloned()
            .collect::<Vec<_>>();
        for id in ids {
            if let Some((file, _)) = self.files.get(id) {
                let mut file = file.lock().expect("file monitor");
                if !file.paused && !file.settled {
                    file.paused = true;
                    paused.push(id.clone());
                    if let Some(snapshot) = self
                        .file_snapshots
                        .lock()
                        .expect("file snapshots")
                        .get_mut(id)
                    {
                        snapshot.paused = true;
                    }
                }
            }
        }
        paused.extend(ids.iter().filter(|id| self.rpc.pause(id)).cloned());
        drop(records);
        self.publish_state();
        paused
    }
    pub fn resume(&self, ids: Option<&[String]>) -> Vec<(String, usize)> {
        let mut records = self.records.lock().expect("monitor records");
        let ids = ids.map(<[String]>::to_vec).unwrap_or_else(|| {
            records
                .keys()
                .chain(self.files.keys())
                .cloned()
                .chain(self.rpc.keys())
                .collect()
        });
        let mut resumed = vec![];
        let mut pending_events = vec![];
        for id in ids {
            if let Some(dropped) = records.get_mut(&id).and_then(CommandMonitor::resume) {
                resumed.push((id, dropped));
                continue;
            }
            if let Some(dropped) = self.rpc.resume(&id) {
                resumed.push((id, dropped));
                continue;
            }
            if let Some((file, _)) = self.files.get(&id) {
                let (events, settled) = {
                    let mut file = file.lock().expect("file monitor");
                    if !file.paused || file.settled {
                        continue;
                    }
                    file.paused = false;
                    let events = file.check().unwrap_or_else(|error| {
                        file.stop(&format!("watcher error: {error}"))
                            .into_iter()
                            .collect()
                    });
                    (events, file.settled)
                };
                let mut snapshots = self.file_snapshots.lock().expect("file snapshots");
                if settled {
                    snapshots.shift_remove(&id);
                } else if let Some(snapshot) = snapshots.get_mut(&id) {
                    snapshot.paused = false;
                    if let Some(window) = &mut snapshot.fire_window {
                        window.count = 0;
                    }
                }
                resumed.push((id, 0));
                pending_events.extend(events);
            }
        }
        drop(records);
        self.publish_state();
        for event in pending_events {
            (self.emit)(event);
        }
        resumed
    }
    pub fn register(
        &mut self,
        runtime: &crate::runtime_session::TerminalRuntimeSession,
        mut record: CommandMonitor,
    ) -> Result<(), crate::runtime_session::RuntimeError> {
        if self.disposed {
            return Err(crate::runtime_session::RuntimeError::RegistryDisposed);
        }
        let (history, mut output) = runtime.subscribe_output()?;
        let mut exit = runtime.subscribe_exit();
        record.snapshot.started_at_ms = now_ms();
        let id = record.snapshot.id.clone();
        let initial = record.consume(&history, now_ms(), &self.runaway_guard());
        self.records
            .lock()
            .expect("monitor records")
            .insert(id.clone(), record);
        self.publish_state();
        for event in initial {
            (self.emit)(event);
        }
        let guard = self.runaway_guard.clone();
        let records = self.records.clone();
        let emit = self.emit.clone();
        let files = self.file_snapshots.clone();
        let rpc_slot = self.rpc_slot.clone();
        let transitions = self.transitions.clone();
        let ended = self.ended.clone();
        self.tasks.retain(|task| !task.is_finished());
        self.tasks.push(tokio::spawn(async move {
            loop {
                let settled=exit.borrow().clone();
                if let Some(result)=settled {
                    let Some(mut record)=records.lock().expect("monitor records").shift_remove(&id) else {return;};
                    publish_snapshot(&records,&files,&rpc_slot,&transitions);
                    while let Ok(chunk)=output.try_recv() {let current=*guard.lock().expect("runaway guard");for event in record.consume(&chunk,now_ms(),&current) {emit(event);}}
                    let (reason,exit_code)=match &result {Ok(exit)=>(if exit.timed_out {MonitorEndedReason::Timeout} else if exit.cancelled {MonitorEndedReason::Killed} else {MonitorEndedReason::Exit},exit.exit_code),Err(_)=>(MonitorEndedReason::Exit,None)};
                    // A stream watch (`tail -F | grep`) that already delivered matches never exits on its own, so
                    // reaching its deadline is the expected end, not a failure: settle it without a wake-up summary.
                    // The ended event below still reports the timeout. A watch that matched nothing keeps the summary.
                    let quiet=matches!(&result,Ok(exit) if exit.timed_out) && record.snapshot.fire_count.unwrap_or(0)>0;
                    if quiet {record.dispose();} else {
                        // A timed-out watch was KILLED by its own timeout_ms deadline; say so instead of a
                        // passive "timed_out". The exit code is deliberately omitted in this branch: a
                        // killed child's code is noise, and "killed at deadline" names the cause instead.
                        let summary=match result {Ok(result)=>{let status=crate::tools::spawn::describe_exit(Some(&result)).unwrap_or_else(||"exited".to_owned());if result.timed_out {format!("watcher killed at deadline ({status})")} else {format!("watcher {status}{}",result.exit_code.map_or(String::new(),|code|format!(" (exit code {code})")))}},Err(error)=>format!("watcher error: {error}")};
                        if let Some(event)=record.settle(summary,now_ms()) {emit(event);}
                    }
                    emit_ended(&ended,&record.snapshot,reason,exit_code,now_ms());
                    return;
                }
                tokio::select! {
                    chunk=output.recv()=>{let Some(chunk)=chunk else {return;};let events=records.lock().expect("monitor records").get_mut(&id).map(|record|{let current=*guard.lock().expect("runaway guard");record.consume(&chunk,now_ms(),&current)}).unwrap_or_default();if !events.is_empty() {publish_snapshot(&records,&files,&rpc_slot,&transitions);}for event in events {emit(event);}},
                    changed=exit.changed()=>{if changed.is_err() {return;}}
                }
            }
        }));
        Ok(())
    }
    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.disposed = true;
        self.registrations.finish_all();
        let now = now_ms();
        let mut events = vec![];
        for (_, (file, task)) in std::mem::take(&mut self.files) {
            task.abort();
            if let Some(event) = file.lock().expect("file monitor").stop("watcher disposed") {
                events.push(event);
            }
        }
        for snapshot in self.file_snapshots.lock().expect("file snapshots").values() {
            emit_ended(
                &self.ended,
                snapshot,
                MonitorEndedReason::Disposed,
                None,
                now,
            );
        }
        self.file_snapshots.lock().expect("file snapshots").clear();
        for task in self.tasks.drain(..) {
            task.abort();
        }
        for record in self.records.lock().expect("monitor records").values() {
            emit_ended(
                &self.ended,
                &record.snapshot,
                MonitorEndedReason::Disposed,
                None,
                now,
            );
        }
        self.records.lock().expect("monitor records").clear();
        self.rpc.dispose();
        self.publish_state();
        for event in events {
            (self.emit)(event);
        }
        self.delivery
            .lock()
            .expect("monitor routing")
            .pending
            .clear();
    }
}
impl Drop for MonitorRegistry {
    fn drop(&mut self) {
        self.dispose();
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    #[tokio::test]
    async fn disposed_registry_rejects_new_file_and_command_watches() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = MonitorRegistry::new(|_| {});
        registry.dispose();
        registry.dispose();
        assert_eq!(
            registry
                .register_file(
                    "file",
                    &dir.path().join("file"),
                    crate::terminal_manifest_model::FileEvent::Create,
                    1000
                )
                .unwrap_err()
                .to_string(),
            "Cannot create file monitor: monitor registry is disposed."
        );
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "read",
            crate::shared::test_sh_options("read value"),
        )
        .unwrap();
        assert!(matches!(
            registry.register(
                &runtime,
                CommandMonitor::new(MonitorSnapshotEntry::default(), None)
            ),
            Err(crate::runtime_session::RuntimeError::RegistryDisposed)
        ));
        assert!(registry.snapshot().is_empty());
        runtime.dispose().unwrap();
    }
    #[test]
    fn detached_delivery_bounds_and_rebinds_events_in_order() {
        let registry = MonitorRegistry::new(|_| panic!("old owner received parked event"));
        registry.detach_delivery();
        for index in 0..105 {
            (registry.emit)(MonitorEvent::Line {
                id: "bash_1".to_owned(),
                description: "watch".to_owned(),
                line: index.to_string(),
                human_input: false,
            });
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        registry.bind_delivery(move |event| {
            sender.send(event).unwrap();
        });
        let events = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(events.len(), 100);
        assert!(matches!(&events[0],MonitorEvent::Line {line,..} if line=="5"));
        assert!(matches!(&events[99],MonitorEvent::Line {line,..} if line=="104"));
    }
    #[tokio::test]
    async fn snapshot_preserves_registration_order_beyond_nine_file_ids() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = MonitorRegistry::new(|_| {});
        let mut ids = vec![];
        for index in 0..12 {
            let (id, _) = registry
                .register_persistent_file(
                    &format!("watch {index}"),
                    &dir.path().join(format!("file{index}")),
                    crate::terminal_manifest_model::FileEvent::Create,
                )
                .unwrap();
            ids.push(id);
        }
        assert_eq!(
            registry
                .snapshot()
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        registry.stop_file(&ids[3]);
        ids.remove(3);
        assert_eq!(
            registry
                .snapshot()
                .iter()
                .map(|entry| entry.id.clone())
                .collect::<Vec<_>>(),
            ids
        );
        registry.dispose();
    }
    #[tokio::test]
    async fn file_disposal_emits_summary_once_after_releasing_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let mut manager = crate::manager::TerminalManager::default();
        let (id, _) = registry
            .register_persistent_file(
                "watch",
                &dir.path().join("file"),
                crate::terminal_manifest_model::FileEvent::Create,
            )
            .unwrap();
        registry.reserve_file_capacity(&id, manager.reserve().unwrap().unwrap());
        registry.dispose();
        assert_eq!(manager.active_size().unwrap(), 0);
        assert!(registry.snapshot().is_empty());
        assert!(
            matches!(events.try_recv(),Ok(MonitorEvent::Summary {summary,..}) if summary=="watcher disposed")
        );
        registry.dispose();
        assert!(events.try_recv().is_err());
    }
    #[tokio::test(start_paused = true)]
    async fn paused_file_preserves_checkpoint_and_resume_checks_immediately() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        std::fs::write(&path, b"old").unwrap();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let (id, _) = registry
            .register_persistent_file(
                "watch",
                &path,
                crate::terminal_manifest_model::FileEvent::Modify,
            )
            .unwrap();
        let saved = registry.file_checkpoint(&id).unwrap();
        registry.pause(std::slice::from_ref(&id));
        std::fs::write(&path, b"new content").unwrap();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;
        assert_eq!(registry.file_checkpoint(&id), Some(saved));
        assert!(events.try_recv().is_err());
        registry.resume(Some(std::slice::from_ref(&id)));
        assert!(matches!(events.try_recv(), Ok(MonitorEvent::Line { .. })));
        assert!(matches!(
            events.try_recv(),
            Ok(MonitorEvent::Summary { .. })
        ));
        assert!(registry.snapshot().is_empty());
        registry.dispose();
    }
    /// Upstream settles a file watch on its watcher's `error` event at once, paused or not.
    #[tokio::test]
    async fn a_watcher_error_settles_a_paused_file_watch() {
        let dir = tempfile::tempdir().unwrap();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let (id, _) = registry
            .register_persistent_file(
                "watch",
                &dir.path().join("file"),
                crate::terminal_manifest_model::FileEvent::Create,
            )
            .unwrap();
        registry.pause(std::slice::from_ref(&id));
        (registry.watcher_errors[&id])("watcher error: boom".to_owned());
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
            .await
            .unwrap();
        assert!(
            matches!(&event, Some(MonitorEvent::Summary { summary, .. }) if summary == "watcher error: boom"),
            "{event:?}"
        );
        assert!(registry.snapshot().is_empty());
        registry.dispose();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_changed_parent_settles_with_a_single_watcher_error_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let (first, second) = (root.join("first"), root.join("second"));
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        std::fs::write(first.join("file"), b"first").unwrap();
        std::fs::write(second.join("file"), b"second").unwrap();
        let parent = root.join("parent");
        std::os::unix::fs::symlink(&first, &parent).unwrap();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let (id, _) = registry
            .register_persistent_file(
                "watch",
                &parent.join("file"),
                crate::terminal_manifest_model::FileEvent::Modify,
            )
            .unwrap();
        registry.pause(std::slice::from_ref(&id));
        std::fs::remove_file(&parent).unwrap();
        std::os::unix::fs::symlink(&second, &parent).unwrap();
        registry.resume(Some(std::slice::from_ref(&id)));
        let expected = format!(
            "watcher error: monitored parent changed: {}",
            parent.display()
        );
        assert!(matches!(
            events.try_recv(),
            Ok(MonitorEvent::Summary { summary, .. }) if summary == expected
        ));
        registry.dispose();
    }
    #[tokio::test]
    async fn state_subscription_observes_registration_pause_rearm_and_stop() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry = MonitorRegistry::new(|_| {});
        let mut state = registry.subscribe_state();
        assert!(state.borrow_and_update().is_empty());
        let (id, _) = registry
            .register_persistent_file(
                "watch",
                &dir.path().join("file"),
                crate::terminal_manifest_model::FileEvent::Create,
            )
            .unwrap();
        state.changed().await.unwrap();
        assert_eq!(state.borrow_and_update()[0].id, id);
        registry.pause(std::slice::from_ref(&id));
        state.changed().await.unwrap();
        assert!(state.borrow_and_update()[0].paused);
        registry.resume(Some(std::slice::from_ref(&id)));
        state.changed().await.unwrap();
        assert!(!state.borrow_and_update()[0].paused);
        assert!(registry.stop_file(&id));
        state.changed().await.unwrap();
        assert!(state.borrow_and_update().is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn file_timeout_starts_at_registration_not_task_first_poll() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("created");
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        registry
            .register_file(
                "created",
                &path,
                crate::terminal_manifest_model::FileEvent::Create,
                1000,
            )
            .unwrap();
        tokio::time::advance(std::time::Duration::from_millis(1000)).await;
        assert!(
            matches!(events.recv().await,Some(MonitorEvent::Summary {summary,..}) if summary=="watcher timed_out")
        );
        assert!(registry.snapshot().is_empty());
    }
    #[tokio::test(start_paused = true)]
    async fn parked_files_do_not_read_changes_and_unpark_preserves_explicit_mute() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("created");
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let (active, _) = registry
            .register_persistent_file(
                "active",
                &path,
                crate::terminal_manifest_model::FileEvent::Create,
            )
            .unwrap();
        let (muted, _) = registry
            .register_persistent_file(
                "muted",
                &path,
                crate::terminal_manifest_model::FileEvent::Create,
            )
            .unwrap();
        registry.pause(std::slice::from_ref(&muted));
        registry.park();
        std::fs::write(&path, b"ready").unwrap();
        tokio::time::advance(std::time::Duration::from_millis(1000)).await;
        assert!(events.try_recv().is_err());
        assert_eq!(registry.snapshot().len(), 2);
        registry.unpark();
        assert!(matches!(events.recv().await,Some(MonitorEvent::Line {id,..}) if id==active));
        assert!(matches!(events.recv().await,Some(MonitorEvent::Summary {id,..}) if id==active));
        assert_eq!(registry.snapshot().len(), 1);
        assert!(registry.snapshot()[0].paused);
        assert_eq!(registry.snapshot()[0].id, muted);
        registry.dispose();
    }
    #[tokio::test]
    async fn file_rearm_callbacks_can_read_command_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("created");
        let mut registry = MonitorRegistry::new(|_| {});
        let records = registry.records.clone();
        registry.emit = std::sync::Arc::new(move |_| {
            assert!(records.try_lock().is_ok());
        });
        let (id, _) = registry
            .register_file(
                "created",
                &path,
                crate::terminal_manifest_model::FileEvent::Create,
                5000,
            )
            .unwrap();
        registry.pause(std::slice::from_ref(&id));
        std::fs::write(&path, b"ready").unwrap();
        assert_eq!(
            registry.resume(Some(std::slice::from_ref(&id))),
            vec![(id, 0)]
        );
        assert!(registry.snapshot().is_empty());
    }
    #[tokio::test]
    async fn native_file_registration_emits_once_and_releases_live_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("created");
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let (id, _) = registry
            .register_file(
                "created",
                &path,
                crate::terminal_manifest_model::FileEvent::Create,
                5000,
            )
            .unwrap();
        assert_eq!(registry.snapshot().len(), 1);
        std::fs::write(&path, b"ready").unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5),async {assert!(matches!(events.recv().await,Some(MonitorEvent::Line {..})));assert!(matches!(events.recv().await,Some(MonitorEvent::Summary {summary,..}) if summary=="watcher completed"));}).await.unwrap();
        assert!(registry.snapshot().is_empty());
        registry.stop_file(&id);
        assert!(events.try_recv().is_err());
    }
    #[tokio::test]
    async fn native_watch_emits_lines_then_exactly_one_completion() {
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "printf ready",
            crate::shared::test_sh_options("stty -echo; printf 'ready\\n'"),
        )
        .unwrap();
        registry
            .register(
                &runtime,
                CommandMonitor::new(
                    MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        description: "ready".to_owned(),
                        ..Default::default()
                    },
                    None,
                ),
            )
            .unwrap();
        let observed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let line = events.recv().await.unwrap();
            let summary = events.recv().await.unwrap();
            (line, summary)
        })
        .await
        .unwrap();
        assert!(matches!(observed.0,MonitorEvent::Line {line,..} if line=="ready"));
        assert!(
            matches!(observed.1,MonitorEvent::Summary {summary,..} if summary=="watcher completed (exit code 0)")
        );
        assert!(registry.snapshot().is_empty());
        assert!(events.try_recv().is_err());
        runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn command_completion_emits_one_ended_event_with_exit_reason() {
        let (ended_sender, mut ended) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new_with_ended(
            |_| {},
            move |event| {
                ended_sender.send(event).unwrap();
            },
        );
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "printf ready",
            crate::shared::test_sh_options("stty -echo; printf 'ready\\n'"),
        )
        .unwrap();
        registry
            .register(
                &runtime,
                CommandMonitor::new(
                    MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        description: "ready".to_owned(),
                        ..Default::default()
                    },
                    None,
                ),
            )
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), ended.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (
                event.id.as_str(),
                event.description.as_str(),
                event.reason,
                event.exit_code
            ),
            ("bash_1", "ready", MonitorEndedReason::Exit, Some(0))
        );
        assert!(event.fire_count >= 1);
        assert!(ended.try_recv().is_err());
        registry.dispose();
        runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn a_watch_killed_by_its_own_deadline_says_killed_at_deadline_without_exit_code() {
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (ended_sender, mut ended) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new_with_ended(
            move |event| {
                sender.send(event).unwrap();
            },
            move |event| {
                ended_sender.send(event).unwrap();
            },
        );
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "sleep",
            crate::shared::test_sh_options("stty -echo; sleep 30")
                .timeout(std::time::Duration::from_millis(200)),
        )
        .unwrap();
        registry
            .register(
                &runtime,
                CommandMonitor::new(
                    MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        description: "deadline".to_owned(),
                        ..Default::default()
                    },
                    None,
                ),
            )
            .unwrap();
        let summary = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Some(MonitorEvent::Summary { summary, .. }) = events.recv().await {
                    return summary;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(summary, "watcher killed at deadline (timed_out)");
        // Structural guard, not prose: the timed-out branch omits the "(exit code N)" suffix.
        assert!(!summary.contains("exit code"));
        // The machine-read outcome: a timed watch ends with reason "timeout" exactly once, and the
        // ending is emitted before the registry is disposed.
        let first = tokio::time::timeout(std::time::Duration::from_secs(5), ended.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first.reason, MonitorEndedReason::Timeout);
        assert_eq!(first.id, "bash_1");
        registry.dispose();
        let mut timeouts = 1;
        while let Ok(event) = ended.try_recv() {
            timeouts += usize::from(event.reason == MonitorEndedReason::Timeout);
        }
        assert_eq!(timeouts, 1);
        runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn deadline_after_delivered_lines_ends_without_a_wake_summary() {
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (ended_sender, mut ended) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new_with_ended(
            move |event| {
                sender.send(event).unwrap();
            },
            move |event| {
                ended_sender.send(event).unwrap();
            },
        );
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "stream",
            crate::shared::test_sh_options("stty -echo; printf 'ready\\n'; read value")
                // The deadline must land after the line: a ConPTY shell on a loaded Windows runner
                // can take longer than a sub-second deadline to print anything at all.
                .timeout(std::time::Duration::from_secs(10)),
        )
        .unwrap();
        registry
            .register(
                &runtime,
                CommandMonitor::new(
                    MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        ..Default::default()
                    },
                    None,
                ),
            )
            .unwrap();
        // Await the delivered line itself before the deadline, never a fixed wait.
        let line = tokio::time::timeout(std::time::Duration::from_secs(60), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(line,MonitorEvent::Line {line,..} if line=="ready"));
        let event = tokio::time::timeout(std::time::Duration::from_secs(60), ended.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.reason, MonitorEndedReason::Timeout);
        // The ended event is emitted after every line and summary of the watch, so nothing else came.
        assert!(events.try_recv().is_err());
        assert!(registry.snapshot().is_empty());
        registry.dispose();
        runtime.dispose().unwrap();
    }
    fn file_request(
        dir: &std::path::Path,
        event: crate::terminal_manifest_model::FileEvent,
    ) -> crate::file_monitor_source::FileRegistrationRequest {
        crate::file_monitor_source::FileRegistrationRequest {
            description: "pending".to_owned(),
            path: "artifact".into(),
            cwd: dir.to_path_buf(),
            event,
            approved_parent: None,
        }
    }
    /// terminal-monitor.test.ts "holds native-watch capacity until the watch settles".
    #[tokio::test]
    async fn a_guarded_registration_holds_capacity_until_the_watch_settles() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = crate::manager::TerminalManager::new(1);
        let registry = std::sync::Mutex::new(MonitorRegistry::new(|_| {}));
        let (id, monitor_id) = MonitorRegistry::register_file_guarded(
            &registry,
            file_request(
                dir.path(),
                crate::terminal_manifest_model::FileEvent::Create,
            ),
            manager.reserve().unwrap(),
            5_000,
            (5_000, false),
            None,
        )
        .await
        .unwrap();
        assert!(id.starts_with("watch_") && monitor_id.starts_with("mon_"));
        assert!(manager.reserve().unwrap().is_none());
        assert_eq!(registry.lock().unwrap().stop_all_files(), 1);
        assert_eq!(manager.active_size().unwrap(), 0);
        registry.lock().unwrap().dispose();
    }
    /// terminal-monitor.test.ts "aborts a pending registration when disposed and releases
    /// capacity": a proof that completes after the dispose is refused and frees its slot.
    #[tokio::test]
    async fn dispose_aborts_a_pending_registration_and_releases_capacity() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("artifact"), b"before").unwrap();
        let mut manager = crate::manager::TerminalManager::new(1);
        let mut registry = MonitorRegistry::new(|_| {});
        let (guard, pending) = registry
            .begin_file_registration(manager.reserve().unwrap(), 5_000)
            .unwrap();
        registry.dispose();
        assert_eq!(manager.active_size().unwrap(), 0);
        let proven = crate::file_monitor_source::prove_file_registration(
            &guard,
            &pending,
            file_request(
                dir.path(),
                crate::terminal_manifest_model::FileEvent::Modify,
            ),
        )
        .await;
        let error = registry
            .finish_file_registration(&pending, proven, (5_000, false), None)
            .unwrap_err()
            .to_string();
        assert!(error.contains("disposed"), "{error}");
        assert!(registry.snapshot().is_empty());
        assert!(manager.reserve().unwrap().is_some());
    }
    /// terminal-monitor.test.ts "aborts all pending registrations when stopAllFiles is called":
    /// the registry stays usable, but the registration in flight is refused.
    #[tokio::test]
    async fn stop_all_files_aborts_pending_registrations() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = crate::manager::TerminalManager::new(1);
        let mut registry = MonitorRegistry::new(|_| {});
        let (guard, pending) = registry
            .begin_file_registration(manager.reserve().unwrap(), 5_000)
            .unwrap();
        assert_eq!(registry.stop_all_files(), 0);
        assert_eq!(manager.active_size().unwrap(), 0);
        let proven = crate::file_monitor_source::prove_file_registration(
            &guard,
            &pending,
            file_request(
                dir.path(),
                crate::terminal_manifest_model::FileEvent::Create,
            ),
        )
        .await;
        let error = registry
            .finish_file_registration(&pending, proven, (5_000, false), None)
            .unwrap_err()
            .to_string();
        assert_eq!(error, crate::file_monitor_guard::REGISTRATION_ABORTED);
        assert!(registry.snapshot().is_empty());
        // A fresh registration after stopAllFiles still succeeds.
        let (guard, pending) = registry
            .begin_file_registration(manager.reserve().unwrap(), 5_000)
            .unwrap();
        let proven = crate::file_monitor_source::prove_file_registration(
            &guard,
            &pending,
            file_request(
                dir.path(),
                crate::terminal_manifest_model::FileEvent::Create,
            ),
        )
        .await;
        registry
            .finish_file_registration(&pending, proven, (5_000, false), None)
            .unwrap();
        assert_eq!(registry.snapshot().len(), 1);
        assert_eq!(manager.active_size().unwrap(), 1);
        registry.dispose();
        assert_eq!(manager.active_size().unwrap(), 0);
    }
    /// A registration whose deadline passes before its proof completes is refused (the paused
    /// clock is the injected time; no sleep).
    #[tokio::test(start_paused = true)]
    async fn a_registration_past_its_deadline_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut manager = crate::manager::TerminalManager::new(1);
        let mut registry = MonitorRegistry::new(|_| {});
        let (guard, pending) = registry
            .begin_file_registration(manager.reserve().unwrap(), 100)
            .unwrap();
        let proven = crate::file_monitor_source::prove_file_registration(
            &guard,
            &pending,
            file_request(
                dir.path(),
                crate::terminal_manifest_model::FileEvent::Create,
            ),
        )
        .await;
        tokio::time::advance(std::time::Duration::from_millis(100)).await;
        let error = registry
            .finish_file_registration(&pending, proven, (5_000, false), None)
            .unwrap_err()
            .to_string();
        assert_eq!(error, crate::file_monitor_guard::REGISTRATION_ABORTED);
        assert_eq!(manager.active_size().unwrap(), 0);
        registry.dispose();
    }
    /// The native parent watcher settles a watch whose parent was retargeted (upstream "fails
    /// closed when an approved symlink parent is retargeted after activation").
    #[cfg(unix)]
    #[tokio::test]
    async fn a_retargeted_parent_settles_the_watch_without_a_line() {
        let root = tempfile::tempdir().unwrap();
        let internal = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let link = root.path().join("parent");
        std::os::unix::fs::symlink(internal.path(), &link).unwrap();
        registry
            .register_file(
                "drift",
                &link.join("artifact"),
                crate::terminal_manifest_model::FileEvent::Create,
                5_000,
            )
            .unwrap();
        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(external.path(), &link).unwrap();
        std::fs::write(external.path().join("artifact"), b"external").unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(&event, MonitorEvent::Summary { summary, .. } if summary.contains("monitored parent changed")),
            "{event:?}"
        );
        assert!(registry.snapshot().is_empty());
    }
    #[tokio::test]
    async fn deadline_without_matches_still_reports_timed_out() {
        let (sender, mut events) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new(move |event| {
            sender.send(event).unwrap();
        });
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "gate",
            crate::shared::test_sh_options("read value")
                .timeout(std::time::Duration::from_millis(500)),
        )
        .unwrap();
        registry
            .register(
                &runtime,
                CommandMonitor::new(
                    MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        ..Default::default()
                    },
                    Some(fancy_regex::Regex::new("^READY$").unwrap()),
                ),
            )
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(event,MonitorEvent::Summary {summary,..} if summary.starts_with("watcher killed at deadline"))
        );
        registry.dispose();
        runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn dispose_emits_disposed_ending_for_every_live_record() {
        let (ended_sender, ended) = std::sync::mpsc::channel();
        let mut registry = MonitorRegistry::new_with_ended(
            |_| {},
            move |event| {
                ended_sender.send(event).unwrap();
            },
        );
        let runtime = crate::runtime_session::TerminalRuntimeSession::start(
            "read value",
            crate::shared::test_sh_options("read value"),
        )
        .unwrap();
        registry
            .register(
                &runtime,
                CommandMonitor::new(
                    MonitorSnapshotEntry {
                        id: "bash_1".to_owned(),
                        ..Default::default()
                    },
                    None,
                ),
            )
            .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let (file_id, _) = registry
            .register_persistent_file(
                "watch",
                &dir.path().join("file"),
                crate::terminal_manifest_model::FileEvent::Create,
            )
            .unwrap();
        registry.dispose();
        let events = ended.try_iter().collect::<Vec<_>>();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.reason == MonitorEndedReason::Disposed)
                .count(),
            2
        );
        assert!(events.iter().any(|event| event.id == "bash_1"));
        assert!(events.iter().any(|event| event.id == file_id));
        runtime.dispose().unwrap();
    }
    #[tokio::test]
    async fn persistent_file_watcher_expiry_reports_timeout_reason() {
        let (ended_sender, mut ended) = tokio::sync::mpsc::unbounded_channel();
        let mut registry = MonitorRegistry::new_with_ended(
            |_| {},
            move |event| {
                ended_sender.send(event).unwrap();
            },
        );
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("watched");
        std::fs::write(&path, b"initial").unwrap();
        let manifest = crate::restore::parse_terminal_manifest(
            &serde_json::json!({
                "monitors": [{
                    "monitorId": "mon_expiring",
                    "sessionId": "s",
                    "description": "watch expiry",
                    "runtimeKind": "file",
                    "durabilityClass": "checkpointed-file",
                    "path": "watched",
                    "cwd": dir.path().to_string_lossy(),
                    "event": "modify",
                    "createdAt": 1,
                    "expiresAt": 10.0,
                    "persistent": true,
                    "suspended": false,
                    "lastCheckpoint": crate::durable_file::file_checkpoint(&path).unwrap(),
                    "deliveryPaused": false,
                    "fireWindow": {"startMs": 1, "count": 0}
                }],
                "backgroundSessions": [],
                "updatedAt": 1
            }),
            "s",
        )
        .unwrap();
        let monitor = &manifest.monitors[0];
        let (runtime_id, _) = registry
            .restore_persistent_file(monitor, &path, 0.0)
            .unwrap();
        let event = tokio::time::timeout(std::time::Duration::from_secs(5), ended.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event.id, runtime_id);
        assert_eq!(event.reason, MonitorEndedReason::Timeout);
        registry.dispose();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stable_identity_encodes_eighty_random_bits() {
        assert_eq!(monitor_id_from_bytes([0; 10]), "mon_0000000000000000");
        assert_eq!(monitor_id_from_bytes([255; 10]), "mon_ZZZZZZZZZZZZZZZZ");
        assert_eq!(allocate_monitor_id().unwrap().len(), 20);
    }
    fn monitor() -> CommandMonitor {
        CommandMonitor::new(
            MonitorSnapshotEntry {
                id: "bash_1".to_owned(),
                description: "ready".to_owned(),
                fire_window: Some(MonitorFireWindow {
                    start_ms: 10.0,
                    count: 0,
                }),
                ..Default::default()
            },
            Some(fancy_regex::Regex::new("^ready").unwrap()),
        )
    }
    #[test]
    fn wake_pacing_applies_only_to_persistent_command_watches() {
        let registry = MonitorRegistry::new(|_| {});
        assert_eq!(
            registry.runaway_guard(),
            crate::settings::TERMINAL_SETTINGS_DEFAULTS.monitor_runaway
        );
        registry.set_runaway_guard(crate::settings::MonitorRunawayGuard {
            lines_per_minute: 3.0,
            minutes: 2.0,
        });
        assert_eq!(registry.runaway_guard().minutes, 2.0);
        let mut persistent = monitor().with_wakes_per_hour(Some(6.0));
        persistent.snapshot.persistent = Some(true);
        registry
            .records
            .lock()
            .unwrap()
            .insert("bash_1".to_owned(), persistent);
        let mut transient = monitor();
        transient.snapshot.id = "bash_2".to_owned();
        registry
            .records
            .lock()
            .unwrap()
            .insert("bash_2".to_owned(), transient);
        assert_eq!(
            registry.wake_pacing("bash_1"),
            Some(WakePacing {
                wakes_per_hour: Some(6.0)
            })
        );
        assert_eq!(registry.wake_pacing("bash_2"), None);
        assert_eq!(registry.wake_pacing("missing"), None);
        registry.records.lock().unwrap().clear();
    }
    #[test]
    fn file_settle_reason_maps_expiry_and_timeout_to_timeout_reason() {
        assert_eq!(
            file_settle_reason("watcher timed_out"),
            MonitorEndedReason::Timeout
        );
        assert_eq!(
            file_settle_reason("watcher expired"),
            MonitorEndedReason::Timeout
        );
        assert_eq!(
            file_settle_reason("watcher killed"),
            MonitorEndedReason::Killed
        );
        assert_eq!(
            file_settle_reason("watcher disposed"),
            MonitorEndedReason::Disposed
        );
        assert_eq!(
            file_settle_reason("watcher completed"),
            MonitorEndedReason::Exit
        );
        assert_eq!(
            file_settle_reason("watcher error: permission denied"),
            MonitorEndedReason::Exit
        );
    }
}
