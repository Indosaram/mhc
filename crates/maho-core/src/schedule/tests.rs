use super::{Result, occurrences::*, runner_lease::*, store::*, types::*};
use std::collections::BTreeMap;

fn input(every_ms: Option<u64>) -> NewScheduledJob {
    NewScheduledJob {
        session_id: "session-a".into(),
        session_file: None,
        cwd: "/work".into(),
        prompt: "stretch".into(),
        due_at: 1_000_000,
        every_ms,
    }
}
struct Probes {
    pid: u32,
    now: u64,
    starts: BTreeMap<u32, u64>,
    groups: Vec<u32>,
}
impl Probes {
    fn new(pid: u32) -> Self {
        Self {
            pid,
            now: 2_000_000,
            starts: BTreeMap::from([(pid, 1_000_000)]),
            groups: Vec::new(),
        }
    }
}
#[async_trait::async_trait]
impl ProcessProbes for Probes {
    fn own_identity(&self) -> RunnerIdentity {
        RunnerIdentity {
            pid: self.pid,
            process_started_at_ms: self.starts[&self.pid],
        }
    }
    fn boot_at_ms(&self) -> u64 {
        100_000
    }
    fn now(&self) -> u64 {
        self.now
    }
    fn pid_alive(&self, pid: u32) -> Result<bool> {
        Ok(self.starts.contains_key(&pid))
    }
    fn delivery_group_alive(&self, pid: u32) -> Result<bool> {
        Ok(self.groups.contains(&pid))
    }
    async fn process_start_ms(&self, pid: u32) -> Result<Option<u64>> {
        Ok(self.starts.get(&pid).copied())
    }
}

#[test]
fn validates_wire_fields_and_utf8_size() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let job = create_scheduled_job(dir.path(), input(None), 10)?;
    let mut raw = serde_json::to_value(&job)?;
    raw["dueAt"] = serde_json::json!(-1);
    assert!(parse_scheduled_job(raw).is_err());
    let mut raw = serde_json::to_value(&job)?;
    raw["prompt"] = serde_json::json!("é".repeat(MAX_PROMPT_BYTES / 2 + 1));
    assert!(parse_scheduled_job(raw).is_err());
    let mut raw = serde_json::to_value(&job)?;
    raw["everyMs"] = serde_json::json!(59_999);
    assert!(parse_scheduled_job(raw).is_err());
    assert_eq!(next_recurring_due_at(100, 60_000, 180_100), 240_100);
    Ok(())
}
#[test]
fn cancellation_is_final_after_claim_and_rearm() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let job = create_scheduled_job(dir.path(), input(Some(60_000)), 10)?;
    let owner = Probes::new(101).own_identity();
    let record = claim_occurrence(dir.path(), &job, &owner)?.expect("claimed");
    let next = ScheduledJob {
        fire_count: 1,
        due_at: 1_060_000,
        ..job.clone()
    };
    rearm_recurring_job(dir.path(), &next)?;
    assert!(cancel_scheduled_job(dir.path(), &job.id, Some("another-session"))?.is_none());
    let cancelled =
        cancel_scheduled_job(dir.path(), &job.id, Some("session-a"))?.expect("cancelled");
    assert!(cancelled.in_flight);
    rearm_recurring_job(dir.path(), &next)?;
    settle_occurrence(dir.path(), &record, &job, 1, Err("delivery failed".into()))?;
    assert!(list_scheduled_jobs(dir.path())?.jobs.is_empty());
    assert!(is_cancelled(dir.path(), &job.id)?);
    Ok(())
}
#[test]
fn stale_generation_claim_restores_new_pending_untouched() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let old = create_scheduled_job(dir.path(), input(Some(60_000)), 10)?;
    let next = ScheduledJob {
        fire_count: 1,
        due_at: 1_060_000,
        ..old.clone()
    };
    rearm_recurring_job(dir.path(), &next)?;
    assert!(claim_occurrence(dir.path(), &old, &Probes::new(101).own_identity())?.is_none());
    let listing = list_scheduled_jobs(dir.path())?;
    assert_eq!(listing.jobs.len(), 1);
    assert_eq!(listing.jobs[0].job, next);
    Ok(())
}
#[test]
fn abandoned_recurring_claim_restores_schedule_not_occurrence() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let job = create_scheduled_job(dir.path(), input(Some(60_000)), 10)?;
    let record =
        claim_occurrence(dir.path(), &job, &Probes::new(101).own_identity())?.expect("claimed");
    let restored = restore_abandoned_schedule(dir.path(), &job, 1, 1_180_000)?.expect("restored");
    assert_eq!(restored.fire_count, 1);
    assert_eq!(restored.due_at, 1_240_000);
    assert!(restore_abandoned_schedule(dir.path(), &job, 1, 1_180_000)?.is_none());
    settle_occurrence(dir.path(), &record, &job, 1, Err("unknown".into()))?;
    assert_eq!(list_scheduled_jobs(dir.path())?.jobs.len(), 2);
    Ok(())
}
#[test]
fn failure_retention_keeps_newest_ten_and_reports_invalid_files() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let job = create_scheduled_job(dir.path(), input(None), 10)?;
    for occurrence in 1..=12 {
        settle_occurrence(
            dir.path(),
            "firing/missing.json",
            &job,
            occurrence,
            Err("failed".into()),
        )?;
    }
    write_atomic(
        &dir.path().join("pending").join("sch_aaaaaaaaaaaa.json"),
        "invalid",
    )?;
    let listing = list_scheduled_jobs(dir.path())?;
    assert_eq!(
        listing
            .jobs
            .iter()
            .filter(|record| record.state == ScheduledJobState::Failed)
            .count(),
        10
    );
    assert!(
        !listing
            .jobs
            .iter()
            .any(|record| record.occurrence == Some(1) || record.occurrence == Some(2))
    );
    assert_eq!(listing.invalid.len(), 1);
    Ok(())
}
#[tokio::test]
async fn leases_reject_reused_pid_and_stale_heartbeat() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let owner = Probes::new(101);
    write_runner_lease(dir.path(), &owner, owner.now, true, None)?;
    assert!(is_watch_runner_available(dir.path(), &owner).await?);
    let stale = Probes {
        now: owner.now + RUNNER_HEARTBEAT_STALE_MS + 1,
        ..Probes::new(101)
    };
    assert!(!is_watch_runner_available(dir.path(), &stale).await?);
    let reused = Probes {
        starts: BTreeMap::from([(101, 1_010_000)]),
        ..Probes::new(101)
    };
    assert!(live_runners(dir.path(), &reused).await?.is_empty());
    assert!(!runners_dir(dir.path()).join("101.json").exists());
    Ok(())
}
#[tokio::test]
async fn orphan_delivery_group_holds_session_until_all_members_exit() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let owner = Probes::new(101);
    let SessionDeliveryLock::Acquired(mut lock) =
        acquire_session_delivery_lock(dir.path(), "a/b", &owner, true, 900_000).await?
    else {
        panic!("expected lock");
    };
    lock.attach_delivery(303, 2_000_000)?;
    let other = Probes {
        groups: vec![303],
        ..Probes::new(202)
    };
    assert!(matches!(
        acquire_session_delivery_lock(dir.path(), "a/b", &other, true, 900_000).await?,
        SessionDeliveryLock::Held {
            held_by_pid: Some(303)
        }
    ));
    let exited = Probes::new(202);
    let SessionDeliveryLock::Acquired(replacement) =
        acquire_session_delivery_lock(dir.path(), "a/b", &exited, true, 900_000).await?
    else {
        panic!("expected reclaimed lock");
    };
    lock.release()?;
    assert!(dir.path().join("sessions/a%2Fb.lock").exists());
    replacement.release()?;
    Ok(())
}
