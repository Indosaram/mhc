//! Scheduled-prompt pass shared by the `schedule run` CLI and a live session (senpi
//! cli/schedule-runner.ts, cli/schedule-delivery.ts types). Lives here so builtin extensions can run
//! the same pass without depending on the CLI crate.
use super::{Result, ScheduleError, occurrences::*, runner_lease::*, store::*, types::*};
use std::{collections::BTreeMap, path::Path};

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledPromptEvent {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub id: String,
    pub session_id: String,
    pub session_file: Option<String>,
    pub cwd: String,
    pub prompt: String,
    pub message: String,
    pub due_at: u64,
    pub fired_at: u64,
    pub every_ms: Option<u64>,
    pub fire_count: u64,
}
#[async_trait::async_trait]
pub trait Delivery: Sync {
    async fn should_defer(
        &self,
        job: &ScheduledJob,
        probes: &dyn ProcessProbes,
    ) -> Result<Option<String>>;
    async fn deliver(
        &self,
        event: &ScheduledPromptEvent,
        lock: &mut DeliveryLock,
        probes: &dyn ProcessProbes,
        timeout_ms: u64,
    ) -> std::result::Result<(), String>;
}
pub fn moved_job_paths(job: &ScheduledJob) -> ScheduledJob {
    ScheduledJob {
        session_file: job
            .session_file
            .as_deref()
            .map(crate::session_manager::resolve_moved_session_path),
        cwd: crate::session_manager::resolve_moved_session_path(&job.cwd),
        ..job.clone()
    }
}

pub const DEFERRED_RETRY_MS: u64 = 15_000;
pub const ABANDONED_OCCURRENCE_ERROR: &str =
    "the runner exited while delivering this occurrence; outcome unknown, not retried";
pub struct RunDueResult {
    pub events: Vec<serde_json::Value>,
    pub next_due_at: Option<u64>,
}
pub struct RunDueOptions<'a, D: Delivery + ?Sized = dyn Delivery + 'a> {
    pub dir: &'a Path,
    pub probes: &'a dyn ProcessProbes,
    pub delivery: &'a D,
    pub concurrency: usize,
    pub delivery_timeout_ms: u64,
    /// Only this session's jobs: the pass a live session runs for itself.
    pub session_id: Option<&'a str>,
}
fn note_next(next: &mut Option<u64>, due: u64) {
    *next = Some(next.map_or(due, |old| old.min(due)));
}

async fn fire_one<D: Delivery + ?Sized>(
    options: &RunDueOptions<'_, D>,
    listed: &ScheduledJob,
) -> Result<Option<serde_json::Value>> {
    let lock = acquire_session_delivery_lock(
        options.dir,
        &listed.session_id,
        options.probes,
        cfg!(unix),
        options.delivery_timeout_ms,
    )
    .await?;
    let mut lock = match lock {
        SessionDeliveryLock::Held { held_by_pid } => {
            let holder =
                held_by_pid.map_or_else(|| "another runner".into(), |pid| format!("process {pid}"));
            return Ok(Some(
                serde_json::json!({"event":"deferred","id":listed.id,"sessionId":listed.session_id,"reason":format!("{holder} is delivering to this session")}),
            ));
        }
        SessionDeliveryLock::Acquired(lock) => lock,
    };
    let result = async {
        let pending = options.dir.join("pending").join(format!("{}.json", listed.id));
        let job = match read_job_file(&pending, &listed.id) {
            Ok(job) => job,
            Err(ScheduleError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if job.due_at > options.probes.now() || is_cancelled(options.dir, &job.id)? { return Ok(None); }
        let delivery_job = moved_job_paths(&job);
        if let Some(reason) = options.delivery.should_defer(&delivery_job, options.probes).await? {
            return Ok(Some(serde_json::json!({"event":"deferred","id":job.id,"sessionId":job.session_id,"reason":reason})));
        }
        let Some(record) = claim_occurrence(options.dir, &job, &options.probes.own_identity())? else { return Ok(None); };
        let occurrence = job.fire_count + 1;
        let fired_at = options.probes.now();
        let next_due_at = job.every_ms.map(|every| next_recurring_due_at(job.due_at, every, fired_at));
        if let Some(due_at) = next_due_at {
            let next = ScheduledJob { fire_count: occurrence, last_fired_at: Some(fired_at), due_at, ..job.clone() };
            if let Err(error) = rearm_recurring_job(options.dir, &next) {
                restore_pending(options.dir, &record, &job.id)?;
                return Err(error);
            }
        }
        let message = format_scheduled_message(&job, fired_at)?;
        let event = ScheduledPromptEvent {
            kind: "scheduled_prompt", id: job.id.clone(), session_id: job.session_id.clone(),
            session_file: delivery_job.session_file, cwd: delivery_job.cwd, prompt: job.prompt.clone(), message,
            due_at: job.due_at, fired_at, every_ms: job.every_ms, fire_count: occurrence,
        };
        let outcome = options.delivery.deliver(&event, &mut lock, options.probes, options.delivery_timeout_ms).await;
        settle_occurrence(options.dir, &record, &ScheduledJob { fire_count: occurrence, last_fired_at: Some(fired_at), ..job.clone() }, occurrence, outcome.clone())?;
        let mut event = serde_json::json!({"event":"fired","id":job.id,"sessionId":job.session_id,"occurrence":occurrence,
            "outcome":if outcome.is_ok() {"delivered"} else {"failed"},"firedAt":fired_at,"dueAt":job.due_at});
        if let Err(error) = outcome { event["error"] = error.into(); }
        if let Some(next) = next_due_at
            && !is_cancelled(options.dir, &job.id)?
        {
            event["nextDueAt"] = next.into();
        }
        Ok(Some(event))
    }.await;
    let released = lock.release();
    match (result, released) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(event), Ok(())) => Ok(event),
    }
}

pub async fn run_due_jobs<D: Delivery + ?Sized>(
    options: &RunDueOptions<'_, D>,
) -> Result<RunDueResult> {
    let listing = list_scheduled_jobs(options.dir)?;
    // Narrow a copy, never the listing: tombstone pruning must see every session's live jobs.
    let jobs: Vec<_> = listing
        .jobs
        .iter()
        .filter(|record| {
            options
                .session_id
                .is_none_or(|id| record.job.session_id == id)
        })
        .cloned()
        .collect();
    let mut events: Vec<_> = listing.invalid.iter().map(|invalid| serde_json::json!({"event":"invalid","state":invalid.state,"file":invalid.file,"error":invalid.error})).collect();
    let mut next_due_at = None;
    if jobs
        .iter()
        .any(|record| record.state == ScheduledJobState::Firing)
    {
        let runners = live_runners(options.dir, options.probes).await?;
        let own = options.probes.own_identity();
        for record in &jobs {
            if record.state != ScheduledJobState::Firing {
                continue;
            }
            let (Some(owner), Some(occurrence)) = (&record.owner, record.occurrence) else {
                continue;
            };
            if *owner == own || is_owner_among(owner, &runners) {
                continue;
            }
            if let Some(restored) = restore_abandoned_schedule(
                options.dir,
                &record.job,
                occurrence,
                options.probes.now(),
            )? {
                note_next(&mut next_due_at, restored.due_at);
            }
            settle_occurrence(
                options.dir,
                &record.file,
                &record.job,
                occurrence,
                Err(ABANDONED_OCCURRENCE_ERROR.into()),
            )?;
            events.push(serde_json::json!({"event":"abandoned","id":record.job.id,"sessionId":record.job.session_id,"occurrence":occurrence,"error":ABANDONED_OCCURRENCE_ERROR}));
        }
    }
    prune_tombstones(options.dir, &listing, options.probes.now())?;
    let mut queues: BTreeMap<String, Vec<ScheduledJob>> = BTreeMap::new();
    for record in jobs {
        if record.state != ScheduledJobState::Pending {
            continue;
        }
        if record.job.due_at > options.probes.now() {
            note_next(&mut next_due_at, record.job.due_at);
            continue;
        }
        if !is_cancelled(options.dir, &record.job.id)? {
            queues
                .entry(record.job.session_id.clone())
                .or_default()
                .push(record.job);
        }
    }
    let queues = std::sync::Mutex::new(
        queues
            .into_values()
            .collect::<std::collections::VecDeque<_>>(),
    );
    let workers = options
        .concurrency
        .min(
            queues
                .lock()
                .map_err(|error| ScheduleError::Invalid(error.to_string()))?
                .len(),
        )
        .max(1);
    let mut tasks = Vec::new();
    for _ in 0..workers {
        let queues = &queues;
        tasks.push(async move {
            let mut events = Vec::new();
            let mut next = None;
            loop {
                let queue = queues.lock().map_err(|error| ScheduleError::Invalid(error.to_string()))?.pop_front();
                let Some(queue) = queue else { break; };
                for job in queue {
                    match fire_one(options, &job).await {
                        Ok(Some(event)) => {
                            if let Some(due) = event["nextDueAt"].as_u64() { note_next(&mut next, due); }
                            let deferred = event["event"] == "deferred";
                            events.push(event);
                            if deferred { note_next(&mut next, options.probes.now() + DEFERRED_RETRY_MS); break; }
                        },
                        Ok(None) => {},
                        Err(error) => {
                            events.push(serde_json::json!({"event":"error","id":job.id,"sessionId":job.session_id,"error":error.to_string()}));
                            note_next(&mut next, options.probes.now() + DEFERRED_RETRY_MS);
                            break;
                        },
                    }
                }
            }
            Ok::<_, ScheduleError>((events, next))
        });
    }
    let results = join_workers(tasks).await;
    for result in results {
        let (batch, next) = result?;
        events.extend(batch);
        if let Some(due) = next {
            note_next(&mut next_due_at, due);
        }
    }
    Ok(RunDueResult {
        events,
        next_due_at,
    })
}

async fn join_workers<F: std::future::Future>(tasks: Vec<F>) -> Vec<F::Output> {
    let mut tasks: Vec<_> = tasks.into_iter().map(|task| Some(Box::pin(task))).collect();
    let mut results: Vec<_> = (0..tasks.len()).map(|_| None).collect();
    std::future::poll_fn(|cx| {
        let mut pending = false;
        for (task, result) in tasks.iter_mut().zip(&mut results) {
            if let Some(future) = task {
                match future.as_mut().poll(cx) {
                    std::task::Poll::Ready(value) => {
                        *result = Some(value);
                        *task = None;
                    }
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    results.into_iter().flatten().collect()
}
