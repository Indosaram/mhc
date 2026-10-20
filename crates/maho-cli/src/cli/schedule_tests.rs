use super::{
    parse, parse_run_options, schedule_delivery::*, schedule_runner::*, schedule_watch::*,
};
use maho_core::schedule::{Result, occurrences::*, runner_lease::*, store::*, types::*};
use std::{path::Path, sync::Mutex, time::Duration};

struct Probes;
#[async_trait::async_trait]
impl ProcessProbes for Probes {
    fn own_identity(&self) -> RunnerIdentity {
        RunnerIdentity {
            pid: 101,
            process_started_at_ms: 1_000_000,
        }
    }
    fn boot_at_ms(&self) -> u64 {
        100_000
    }
    fn now(&self) -> u64 {
        2_000_000
    }
    fn pid_alive(&self, pid: u32) -> Result<bool> {
        Ok(pid == 101)
    }
    fn delivery_group_alive(&self, _pid: u32) -> Result<bool> {
        Ok(false)
    }
    async fn process_start_ms(&self, pid: u32) -> Result<Option<u64>> {
        Ok((pid == 101).then_some(1_000_000))
    }
}
fn create(dir: &Path, session: &str, every: Option<u64>) -> Result<ScheduledJob> {
    create_scheduled_job(
        dir,
        NewScheduledJob {
            session_id: session.into(),
            session_file: None,
            cwd: "/work".into(),
            prompt: "stretch".into(),
            due_at: 1_000_000,
            every_ms: every,
        },
        10,
    )
}
struct Recording {
    events: Mutex<Vec<ScheduledPromptEvent>>,
    failure: bool,
    defer: bool,
}
#[async_trait::async_trait]
impl Delivery for Recording {
    async fn should_defer(
        &self,
        _: &ScheduledJob,
        _: &dyn ProcessProbes,
    ) -> Result<Option<String>> {
        Ok(self.defer.then(|| "session busy".into()))
    }
    async fn deliver(
        &self,
        event: &ScheduledPromptEvent,
        _: &mut DeliveryLock,
        _: &dyn ProcessProbes,
        _: u64,
    ) -> std::result::Result<(), String> {
        self.events.lock().expect("events").push(event.clone());
        if self.failure {
            Err("hook failed".into())
        } else {
            Ok(())
        }
    }
}
fn options<'a, D: Delivery + ?Sized>(dir: &'a Path, delivery: &'a D) -> RunDueOptions<'a, D> {
    RunDueOptions {
        dir,
        probes: &Probes,
        delivery,
        concurrency: 4,
        delivery_timeout_ms: 900_000,
    }
}

struct PathRecording<'a> {
    expected_file: &'a str,
    expected_cwd: &'a str,
    events: Mutex<Vec<ScheduledPromptEvent>>,
}
#[async_trait::async_trait]
impl Delivery for PathRecording<'_> {
    async fn should_defer(&self, job: &ScheduledJob, _: &dyn ProcessProbes) -> Result<Option<String>> {
        assert_eq!(job.session_file.as_deref(), Some(self.expected_file));
        assert_eq!(job.cwd, self.expected_cwd);
        Ok(None)
    }
    async fn deliver(&self, event: &ScheduledPromptEvent, _: &mut DeliveryLock, _: &dyn ProcessProbes, _: u64) -> std::result::Result<(), String> {
        self.events.lock().expect("events").push(event.clone());
        Ok(())
    }
}

#[tokio::test]
async fn moved_paths_reach_guard_and_delivery_without_rewriting_recurring_job() -> Result<()> {
    // Given
    let home = maho_core::config::home_dir_path();
    let root = tempfile::tempdir_in(home)?;
    let root_path = std::fs::canonicalize(root.path())?;
    let old = root_path.join("old");
    let new = root_path.join("new");
    ensure_dir(&old)?;
    ensure_dir(&new)?;
    write_atomic(&old.join("omo-desktop-moved.json"), &serde_json::json!({
        "kind":"omo-desktop-moved", "schemaVersion":1, "homeId":"schedule-fixture",
        "movedTo":new, "moved":["sessions", "worktrees/project"]
    }).to_string())?;
    write_atomic(&new.join("omo-desktop-home.json"), &serde_json::json!({
        "kind":"omo-desktop-data-home", "schemaVersion":1, "appId":"com.omo.desktop", "homeId":"schedule-fixture"
    }).to_string())?;
    let dir = root_path.join("schedule");
    let original_file = old.join("sessions/a.jsonl").to_string_lossy().into_owned();
    let original_cwd = old.join("worktrees/project").to_string_lossy().into_owned();
    let job = create_scheduled_job(&dir, NewScheduledJob {
        session_id:"a".into(), session_file:Some(original_file.clone()), cwd:original_cwd.clone(),
        prompt:"stretch".into(), due_at:1_000_000, every_ms:Some(60_000),
    }, 10)?;
    let expected_file = new.join("sessions/a.jsonl").to_string_lossy().into_owned();
    let expected_cwd = new.join("worktrees/project").to_string_lossy().into_owned();
    let delivery = PathRecording { expected_file:&expected_file, expected_cwd:&expected_cwd, events:Mutex::new(Vec::new()) };
    // When
    let result = run_due_jobs(&options(&dir, &delivery)).await?;
    // Then
    assert_eq!(result.events[0]["outcome"], "delivered");
    let events = delivery.events.lock().expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].session_file.as_deref(), Some(expected_file.as_str()));
    assert_eq!(events[0].cwd, expected_cwd);
    let listing = list_scheduled_jobs(&dir)?;
    let pending = listing.jobs.iter().find(|record| record.job.id == job.id).expect("recurring job");
    assert_eq!(pending.job.session_file.as_deref(), Some(original_file.as_str()));
    assert_eq!(pending.job.cwd, original_cwd);
    Ok(())
}

#[tokio::test]
async fn native_session_holder_defers_default_delivery_until_manager_disposes() -> Result<()> {
    // Given
    let root = tempfile::tempdir()?;
    let directory = root.path().to_string_lossy();
    let mut manager = maho_core::session_manager::SessionManager::create(&directory, Some(&directory), None);
    manager.append_message(serde_json::json!({"role":"assistant", "content":"ready", "timestamp":1}));
    let job = create_scheduled_job(&root.path().join("schedule"), NewScheduledJob {
        session_id:manager.session_id().into(), session_file:manager.session_file().map(str::to_owned), cwd:directory.into_owned(),
        prompt:"stretch".into(), due_at:1_000_000, every_ms:None,
    }, 10)?;
    let probes = NativeProbes::new().await?;
    let delivery = ProcessDelivery { exec:None, executable:std::env::current_exe()? };
    // When
    let deferred = delivery.should_defer(&job, &probes).await?;
    // Then
    assert!(deferred.is_some());
    manager.dispose();
    assert!(delivery.should_defer(&job, &probes).await?.is_none());
    Ok(())
}

#[tokio::test]
async fn exec_delivery_bypasses_session_holder_guard() -> Result<()> {
    // Given
    let root = tempfile::tempdir()?;
    let mut job = create(root.path(), "a", None)?;
    let session = root.path().join("a.jsonl");
    write_atomic(&session, "{}")?;
    write_atomic(&root.path().join("session-holders/a/101.json"), &serde_json::json!({
        "pid":101, "bootAtMs":100_000, "processStartedAtMs":1_000_000
    }).to_string())?;
    job.session_file = Some(session.to_string_lossy().into_owned());
    let delivery = ProcessDelivery { exec:Some("unused".into()), executable:std::env::current_exe()? };
    // When
    let deferred = delivery.should_defer(&job, &Probes).await?;
    // Then
    assert!(deferred.is_none());
    Ok(())
}
#[test]
fn cli_rejects_missing_and_invalid_options() {
    assert!(parse(&[]).is_err());
    assert!(parse(&["cancel".into()]).is_err());
    assert!(parse_run_options(&["--concurrency".into(), "0".into()]).is_err());
    assert!(parse_run_options(&["--exec".into(), " ".into()]).is_err());
    let parsed = parse_run_options(&["--watch".into(), "--concurrency".into(), "2".into()])
        .expect("options");
    assert!(parsed.watch);
    assert_eq!(parsed.concurrency, 2);
}
#[tokio::test]
async fn successful_one_shot_is_delivered_once_and_removed() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let job = create(dir.path(), "a", None)?;
    let delivery = Recording {
        events: Mutex::new(Vec::new()),
        failure: false,
        defer: false,
    };
    let result = run_due_jobs(&options(dir.path(), &delivery)).await?;
    assert_eq!(result.events[0]["outcome"], "delivered");
    assert!(list_scheduled_jobs(dir.path())?.jobs.is_empty());
    assert!(
        run_due_jobs(&options(dir.path(), &delivery))
            .await?
            .events
            .is_empty()
    );
    let events = delivery.events.lock().expect("events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].id, job.id);
    assert_eq!(events[0].fire_count, 1);
    assert_eq!(events[0].prompt, job.prompt);
    Ok(())
}
#[tokio::test]
async fn failed_recurring_delivery_preserves_next_schedule_and_failed_occurrence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    create(dir.path(), "a", Some(60_000))?;
    let delivery = Recording {
        events: Mutex::new(Vec::new()),
        failure: true,
        defer: false,
    };
    let result = run_due_jobs(&options(dir.path(), &delivery)).await?;
    assert_eq!(result.events[0]["outcome"], "failed");
    assert_eq!(result.next_due_at, Some(2_020_000));
    let listing = list_scheduled_jobs(dir.path())?;
    assert_eq!(listing.jobs.len(), 2);
    let pending = listing
        .jobs
        .iter()
        .find(|record| record.state == ScheduledJobState::Pending)
        .expect("pending");
    assert_eq!(pending.job.fire_count, 1);
    assert_eq!(pending.job.last_fired_at, Some(2_000_000));
    assert!(pending.job.last_error.is_none());
    Ok(())
}
#[tokio::test]
async fn deferral_leaves_job_pending_and_uses_retry_time() -> Result<()> {
    let dir = tempfile::tempdir()?;
    create(dir.path(), "a", None)?;
    let delivery = Recording {
        events: Mutex::new(Vec::new()),
        failure: false,
        defer: true,
    };
    let result = run_due_jobs(&options(dir.path(), &delivery)).await?;
    assert_eq!(result.next_due_at, Some(2_000_000 + DEFERRED_RETRY_MS));
    assert_eq!(
        list_scheduled_jobs(dir.path())?.jobs[0].state,
        ScheduledJobState::Pending
    );
    assert!(delivery.events.lock().expect("events").is_empty());
    Ok(())
}
struct CancelOnDelivery<'a> {
    dir: &'a Path,
}
#[async_trait::async_trait]
impl Delivery for CancelOnDelivery<'_> {
    async fn should_defer(
        &self,
        _: &ScheduledJob,
        _: &dyn ProcessProbes,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    async fn deliver(
        &self,
        event: &ScheduledPromptEvent,
        _: &mut DeliveryLock,
        _: &dyn ProcessProbes,
        _: u64,
    ) -> std::result::Result<(), String> {
        cancel_scheduled_job(self.dir, &event.id, None).map_err(|error| error.to_string())?;
        Err("failed after cancellation".into())
    }
}
#[tokio::test]
async fn cancellation_during_delivery_prevents_rearm_and_failure_record() -> Result<()> {
    let dir = tempfile::tempdir()?;
    create(dir.path(), "a", Some(60_000))?;
    let result = run_due_jobs(&options(dir.path(), &CancelOnDelivery { dir: dir.path() })).await?;
    assert!(result.events[0].get("nextDueAt").is_none());
    assert!(list_scheduled_jobs(dir.path())?.jobs.is_empty());
    Ok(())
}

struct Parked {
    started: tokio::sync::mpsc::UnboundedSender<String>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}
#[async_trait::async_trait]
impl Delivery for Parked {
    async fn should_defer(
        &self,
        _: &ScheduledJob,
        _: &dyn ProcessProbes,
    ) -> Result<Option<String>> {
        Ok(None)
    }
    async fn deliver(
        &self,
        event: &ScheduledPromptEvent,
        _: &mut DeliveryLock,
        _: &dyn ProcessProbes,
        _: u64,
    ) -> std::result::Result<(), String> {
        self.started
            .send(event.session_id.clone())
            .map_err(|error| error.to_string())?;
        let release = if event.session_id == "a" {
            self.release.lock().expect("release").take()
        } else {
            None
        };
        if let Some(release) = release {
            release.await.map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}
#[tokio::test]
async fn separate_sessions_run_concurrently_but_same_session_stays_ordered() -> Result<()> {
    let dir = tempfile::tempdir()?;
    create(dir.path(), "a", None)?;
    create(dir.path(), "a", None)?;
    create(dir.path(), "b", None)?;
    let (started, mut starts) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = tokio::sync::oneshot::channel();
    let delivery = Parked {
        started,
        release: Mutex::new(Some(released)),
    };
    let pass_options = options(dir.path(), &delivery);
    let pass = run_due_jobs(&pass_options);
    tokio::pin!(pass);
    let first_two = async {
        let first = starts.recv().await.expect("first delivery");
        let second = starts.recv().await.expect("second delivery");
        assert_ne!(first, second);
        release.send(()).expect("release parked delivery");
    };
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(pass, first_two)
    })
    .await
    .expect("bounded parallel delivery");
    assert_eq!(result?.events.len(), 3);
    assert_eq!(starts.recv().await.as_deref(), Some("a"));
    Ok(())
}

#[tokio::test]
async fn dead_runner_occurrence_is_not_redelivered() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let job = create(dir.path(), "a", Some(60_000))?;
    claim_occurrence(
        dir.path(),
        &job,
        &RunnerIdentity {
            pid: 202,
            process_started_at_ms: 1_000_000,
        },
    )?
    .expect("claim");
    let delivery = Recording {
        events: Mutex::new(Vec::new()),
        failure: false,
        defer: false,
    };
    let result = run_due_jobs(&options(dir.path(), &delivery)).await?;
    assert_eq!(result.events[0]["event"], "abandoned");
    assert!(delivery.events.lock().expect("events").is_empty());
    assert_eq!(result.next_due_at, Some(2_020_000));
    let listing = list_scheduled_jobs(dir.path())?;
    assert_eq!(listing.jobs.len(), 2);
    assert!(
        listing
            .jobs
            .iter()
            .any(|record| record.state == ScheduledJobState::Failed)
    );
    assert!(
        listing
            .jobs
            .iter()
            .any(|record| record.state == ScheduledJobState::Pending && record.job.fire_count == 1)
    );
    Ok(())
}
#[cfg(unix)]
#[tokio::test]
async fn real_exec_hook_receives_json_after_lock_registration() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let output = dir.path().join("event.json");
    let job = create(dir.path(), "a", None)?;
    let native = NativeProbes::new().await?;
    let delivery = ProcessDelivery {
        exec: Some(format!("cat > '{}'", output.display())),
        executable: std::env::current_exe()?,
    };
    let options = RunDueOptions {
        dir: dir.path(),
        probes: &native,
        delivery: &delivery,
        concurrency: 1,
        delivery_timeout_ms: 5000,
    };
    let result = tokio::time::timeout(Duration::from_secs(10), run_due_jobs(&options))
        .await
        .expect("bounded delivery")?;
    assert_eq!(result.events[0]["outcome"], "delivered");
    let event: serde_json::Value = serde_json::from_slice(&std::fs::read(output)?)?;
    assert_eq!(event["type"], "scheduled_prompt");
    assert_eq!(event["id"], job.id);
    assert_eq!(event["prompt"], job.prompt);
    assert!(list_scheduled_jobs(dir.path())?.jobs.is_empty());
    Ok(())
}
