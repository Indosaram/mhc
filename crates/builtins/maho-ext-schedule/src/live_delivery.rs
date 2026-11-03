//! Live delivery (senpi#4074): while a session is open in an interactive or RPC process, that process
//! fires the session's own due jobs and steers them into the running turn (or starts a turn when idle).
//! Runners keep deferring while the session is open, so the live holder is the only writer then.
//!
//! It reuses the runner's pass (`run_due_jobs` scoped to this session): the same session lock, claim
//! rename, re-arm-before-delivery and settle. One timer is armed for the earliest due time; a create or
//! cancel from the tool and any change in `pending/` re-plan it, so nothing polls.
use crate::TimerFactory;
use maho_core::schedule::{
    Result as ScheduleResult,
    native_probes::{NativeProbes, NowFn},
    runner::{DEFERRED_RETRY_MS, Delivery, RunDueOptions, ScheduledPromptEvent, run_due_jobs},
    runner_lease::{DeliveryLock, ProcessProbes, remove_runner_lease, write_runner_lease},
    store::{ensure_dir, schedule_dir},
};
use maho_ext_api::{
    EventKind, EventResult, ExtensionApi, ExtensionFailure, ExtensionMode, NotificationType,
    SendUserMessageOptions, StreamingBehavior, UserMessageContent,
};
use std::{
    collections::HashMap,
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};

pub type PassFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
pub type TimerFire = Arc<dyn Fn() -> PassFuture + Send + Sync>;

/// One-shot timer for the next pass; `fire` resolves when that pass has finished.
pub trait LiveTimerPort: Send + Sync {
    fn arm(&self, delay_ms: u64, fire: TimerFire);
    fn cancel(&self);
}

const MAX_TIMER_DELAY_MS: u64 = (1 << 31) - 1;

#[derive(Default)]
pub struct NodeLiveTimer {
    handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}
impl LiveTimerPort for NodeLiveTimer {
    fn arm(&self, delay_ms: u64, fire: TimerFire) {
        let slot = self.handle.clone();
        let mut guard = self.handle.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(previous) = guard.take() {
            previous.abort();
        }
        *guard = Some(tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(
                delay_ms.min(MAX_TIMER_DELAY_MS),
            ))
            .await;
            // Forget the handle before firing: the pass re-arms through this same timer.
            drop(slot.lock().unwrap_or_else(PoisonError::into_inner).take());
            fire().await;
        }));
    }
    fn cancel(&self) {
        if let Some(handle) = self
            .handle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            handle.abort();
        }
    }
}

// A multi-session host runs several live sessions in one process, and the runner lease is per pid:
// the lease stays while any of them is live, so an outside runner never treats their claims as abandoned.
fn lease_holders() -> &'static Mutex<HashMap<PathBuf, usize>> {
    static HOLDERS: std::sync::OnceLock<Mutex<HashMap<PathBuf, usize>>> =
        std::sync::OnceLock::new();
    HOLDERS.get_or_init(Default::default)
}

type Release = Box<dyn FnOnce() -> ScheduleResult<()> + Send>;

fn hold_lease(dir: PathBuf, probes: Arc<NativeProbes>, now: u64) -> ScheduleResult<Release> {
    let first = {
        let mut holders = lease_holders()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let count = holders.entry(dir.clone()).or_insert(0);
        *count += 1;
        *count == 1
    };
    if first && let Err(error) = write_runner_lease(&dir, probes.as_ref(), now, false, None) {
        let mut holders = lease_holders()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        holders.remove(&dir);
        return Err(error);
    }
    Ok(Box::new(move || {
        let last = {
            let mut holders = lease_holders()
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            let left = holders.get(&dir).copied().unwrap_or(1).saturating_sub(1);
            if left > 0 {
                holders.insert(dir.clone(), left);
                false
            } else {
                holders.remove(&dir);
                true
            }
        };
        if last {
            remove_runner_lease(&dir, &probes.own_identity())
        } else {
            Ok(())
        }
    }))
}

pub(crate) struct LiveSession {
    dir: PathBuf,
    session_id: String,
    timer: Box<dyn LiveTimerPort>,
    probes: Arc<NativeProbes>,
    release: Mutex<Option<Release>>,
    notify: Arc<dyn Fn(&str) + Send + Sync>,
    watcher: Mutex<Option<notify::RecommendedWatcher>>,
    stopped: std::sync::atomic::AtomicBool,
}
impl LiveSession {
    fn stopped(&self) -> bool {
        self.stopped.load(std::sync::atomic::Ordering::SeqCst)
    }
}

struct Inner {
    now: NowFn,
    timer: TimerFactory,
    sender: Arc<ExtensionApi>,
    live: Mutex<Option<Arc<LiveSession>>>,
    /// Passes are serialized; `stop` waits on it to settle a pass in flight.
    tail: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct LiveDelivery {
    inner: Arc<Inner>,
}
impl LiveDelivery {
    /// Whether this process delivers the current session's jobs itself.
    pub fn is_live(&self) -> bool {
        self.inner
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
    }
    /// Run a pass now and re-arm the timer; passes are serialized.
    pub async fn replan(&self) {
        let session = self
            .inner
            .live
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(session) = session {
            replan_for(self.inner.clone(), session).await;
        }
    }
}

struct SteerDelivery {
    sender: Arc<ExtensionApi>,
}
#[async_trait::async_trait]
impl Delivery for SteerDelivery {
    async fn should_defer(
        &self,
        _: &maho_core::schedule::types::ScheduledJob,
        _: &dyn ProcessProbes,
    ) -> ScheduleResult<Option<String>> {
        Ok(None)
    }
    async fn deliver(
        &self,
        event: &ScheduledPromptEvent,
        _: &mut DeliveryLock,
        _: &dyn ProcessProbes,
        _: u64,
    ) -> Result<(), String> {
        self.sender
            .send_user_message(
                UserMessageContent::Text(event.message.clone()),
                SendUserMessageOptions {
                    deliver_as: Some(StreamingBehavior::Steer),
                    expand_prompt_templates: false,
                },
            )
            .map_err(|error| error.message)
    }
}

async fn pass(inner: &Arc<Inner>, session: &Arc<LiveSession>) {
    if session.stopped() {
        return;
    }
    let delivery = SteerDelivery {
        sender: inner.sender.clone(),
    };
    let result = run_due_jobs(&RunDueOptions {
        dir: &session.dir,
        probes: session.probes.as_ref(),
        delivery: &delivery,
        concurrency: 1,
        delivery_timeout_ms: maho_core::schedule::runner_lease::DEFAULT_MAX_DELIVERY_MS,
        session_id: Some(&session.session_id),
    })
    .await;
    let next_due_at = match result {
        Ok(result) => {
            for event in &result.events {
                // A job that failed inside the pass is retried later; the user still hears why it was not delivered.
                if event["event"] == "error"
                    && let Some(id) = event["id"].as_str()
                {
                    (session.notify)(&format!(
                        "Scheduled prompt {id} could not be delivered: {}",
                        event["error"].as_str().unwrap_or("")
                    ));
                }
            }
            result.next_due_at
        }
        Err(error) => {
            // The schedule directory could not be read or written: say so, and try again later rather than drop the jobs.
            (session.notify)(&format!("Scheduled prompts could not be checked: {error}"));
            Some((inner.now)() + DEFERRED_RETRY_MS)
        }
    };
    let Some(next_due_at) = next_due_at else {
        return;
    };
    if session.stopped() {
        return;
    }
    let (next_inner, next_session) = (inner.clone(), session.clone());
    session.timer.arm(
        next_due_at.saturating_sub((inner.now)()),
        Arc::new(move || replan_for(next_inner.clone(), next_session.clone())),
    );
}

fn replan_for(inner: Arc<Inner>, session: Arc<LiveSession>) -> PassFuture {
    Box::pin(async move {
        let _serial = inner.tail.lock().await;
        pass(&inner, &session).await;
    })
}

async fn stop(inner: &Arc<Inner>) {
    let session = inner
        .live
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    let Some(session) = session else { return };
    session
        .stopped
        .store(true, std::sync::atomic::Ordering::SeqCst);
    session.timer.cancel();
    drop(
        session
            .watcher
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(),
    );
    // Settle a pass still in flight first: once the lease is gone an outside runner may treat its claim as abandoned.
    drop(inner.tail.lock().await);
    let release = session
        .release
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take();
    if let Some(release) = release
        && let Err(error) = release()
    {
        (session.notify)(&format!(
            "Scheduled prompts could not release the runner lease: {error}"
        ));
    }
}

pub fn register_live_delivery(
    api: &mut ExtensionApi,
    now: NowFn,
    timer: TimerFactory,
) -> LiveDelivery {
    let sender = Arc::new(ExtensionApi::new(
        api.registered.clone(),
        api.profile.clone(),
        api.events.clone(),
        api.runtime.clone(),
    ));
    let inner = Arc::new(Inner {
        now,
        timer,
        sender,
        live: Mutex::new(None),
        tail: tokio::sync::Mutex::new(()),
    });
    let start = inner.clone();
    api.on(
        EventKind::SessionStart,
        Arc::new(move |_event, ctx| {
            let inner = start.clone();
            Box::pin(async move {
                stop(&inner).await;
                if matches!(ctx.mode, ExtensionMode::Print | ExtensionMode::Json) {
                    return Ok(EventResult::None);
                }
                let dir = schedule_dir(&ctx.agent_dir);
                let pending = dir.join("pending");
                ensure_dir(&pending).map_err(|error| ExtensionFailure::new(error.to_string()))?;
                let probes = Arc::new(
                    NativeProbes::new(inner.now.clone())
                        .map_err(|error| ExtensionFailure::new(error.to_string()))?,
                );
                let release = hold_lease(dir.clone(), probes.clone(), (inner.now)())
                    .map_err(|error| ExtensionFailure::new(error.to_string()))?;
                let ui = ctx.ui.clone();
                let session = Arc::new(LiveSession {
                    dir,
                    session_id: ctx.session_manager.session_id().to_owned(),
                    timer: (inner.timer)(),
                    probes,
                    release: Mutex::new(Some(release)),
                    notify: Arc::new(move |message| ui.notify(message, NotificationType::Warning)),
                    watcher: Mutex::new(None),
                    stopped: std::sync::atomic::AtomicBool::new(false),
                });
                let handle = tokio::runtime::Handle::current();
                let (watch_inner, watch_session) = (inner.clone(), session.clone());
                // Without a watch, jobs written by another process are picked up at the next armed pass.
                if let Ok(mut watcher) =
                    notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                        // fs.watch reports content changes only; inotify also reports the pass's own
                        // directory reads as access events, and replanning on those never settles.
                        if matches!(&event, Ok(event) if event.kind.is_access()) {
                            return;
                        }
                        drop(handle.spawn(replan_for(watch_inner.clone(), watch_session.clone())));
                    })
                {
                    use notify::Watcher;
                    if watcher
                        .watch(&pending, notify::RecursiveMode::NonRecursive)
                        .is_ok()
                    {
                        *session
                            .watcher
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) = Some(watcher);
                    }
                }
                *inner.live.lock().unwrap_or_else(PoisonError::into_inner) = Some(session.clone());
                replan_for(inner.clone(), session).await;
                Ok(EventResult::None)
            })
        }),
    );
    let shutdown = inner.clone();
    api.on(
        EventKind::SessionShutdown,
        Arc::new(move |_event, _ctx| {
            let inner = shutdown.clone();
            Box::pin(async move {
                stop(&inner).await;
                Ok(EventResult::None)
            })
        }),
    );
    LiveDelivery { inner }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::schedule::store::{NewScheduledJob, create_scheduled_job};
    use maho_ext_api::*;

    struct Capture(Mutex<Vec<(String, Option<StreamingBehavior>)>>);
    impl ExtensionActions for Capture {
        fn append_entry(&self, _: &str, _: Option<JsonValue>) -> Result<(), ExtensionFailure> {
            Ok(())
        }
        fn send_user_message(
            &self,
            content: UserMessageContent,
            options: SendUserMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            let UserMessageContent::Text(text) = content else {
                return Err("unexpected content".into());
            };
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((text, options.deliver_as));
            Ok(())
        }
        fn send_message(
            &self,
            _: CustomMessage,
            _: SendMessageOptions,
        ) -> Result<(), ExtensionFailure> {
            Err("unexpected custom message".into())
        }
        fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
            Ok(Vec::new())
        }
    }
    #[derive(Default)]
    struct FakeTimer(Arc<Mutex<Vec<u64>>>);
    impl LiveTimerPort for FakeTimer {
        fn arm(&self, delay_ms: u64, _: TimerFire) {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(delay_ms);
        }
        fn cancel(&self) {}
    }

    #[tokio::test]
    async fn due_job_is_steered_into_the_session_and_the_timer_rearms_for_the_next_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let now = crate::system_now();
        let job = |due_at, every_ms| NewScheduledJob {
            session_id: "live".into(),
            session_file: None,
            cwd: "/work".into(),
            prompt: "stretch".into(),
            due_at,
            every_ms,
        };
        let created = (now)();
        create_scheduled_job(dir.path(), job(created, None), created).expect("due job");
        create_scheduled_job(dir.path(), job(created + 3_600_000, None), created)
            .expect("later job");
        create_scheduled_job(
            dir.path(),
            NewScheduledJob {
                session_id: "other".into(),
                ..job(created, None)
            },
            created,
        )
        .expect("foreign job");
        let capture = Arc::new(Capture(Mutex::new(Vec::new())));
        let runtime = ExtensionRuntime::default();
        runtime.bind(capture.clone());
        let sender = Arc::new(ExtensionApi::new(
            LoadedExtension::new("schedule", "/tmp".into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            runtime,
        ));
        let armed = Arc::new(Mutex::new(Vec::new()));
        let timer_armed = armed.clone();
        let inner = Arc::new(Inner {
            now: now.clone(),
            timer: Arc::new(move || {
                Box::new(FakeTimer(timer_armed.clone())) as Box<dyn LiveTimerPort>
            }),
            sender,
            live: Mutex::new(None),
            tail: tokio::sync::Mutex::new(()),
        });
        let session = Arc::new(LiveSession {
            dir: dir.path().to_path_buf(),
            session_id: "live".into(),
            timer: (inner.timer)(),
            probes: Arc::new(NativeProbes::new(now).expect("probes")),
            release: Mutex::new(None),
            notify: Arc::new(|_| {}),
            watcher: Mutex::new(None),
            stopped: std::sync::atomic::AtomicBool::new(false),
        });
        replan_for(inner.clone(), session.clone()).await;
        let delivered = capture
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        assert_eq!(
            delivered.len(),
            1,
            "only the session's own due job is delivered"
        );
        assert!(delivered[0].0.contains("stretch"));
        assert_eq!(delivered[0].1, Some(StreamingBehavior::Steer));
        let armed = armed.lock().unwrap_or_else(PoisonError::into_inner).clone();
        assert_eq!(armed.len(), 1);
        assert!(
            armed[0] > 3_000_000 && armed[0] <= 3_600_000,
            "timer targets the earliest remaining due time: {armed:?}"
        );
    }
}
