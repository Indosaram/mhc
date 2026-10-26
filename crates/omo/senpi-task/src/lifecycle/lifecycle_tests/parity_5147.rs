use super::*;
use crate::lifecycle::types::SuspendInput;
use crate::state::SuspensionReason;

#[test]
fn session_quit_deferral_is_replaced_but_specific_markers_survive() {
    for reason in [
        None,
        Some(SuspensionReason::SessionQuit),
        Some(SuspensionReason::RevivalDeferred),
        Some(SuspensionReason::HostDraining),
    ] {
        let temp = temp_store();
        let registry = Arc::new(FakeRegistry::default());
        let lifecycle = lifecycle(&temp.store, &registry, default_settings());
        let mut record = seed_record(
            &temp.store,
            Seed {
                task_id: "st_0000a001",
                residency_state: Some(ResidencyState::PersistedOnly),
                ..Default::default()
            },
        );
        record.suspension_reason = reason;
        temp.store.replace(&record).expect("replace");
        crate::lifecycle::host_session_record::mark_revival_deferred(
            lifecycle.context(),
            "st_0000a001",
            "run_paused",
        )
        .expect("mark");
        let fresh = temp
            .store
            .load("st_0000a001")
            .expect("load")
            .expect("record");
        assert_eq!(
            fresh.suspension_reason,
            Some(if reason == Some(SuspensionReason::HostDraining) {
                SuspensionReason::HostDraining
            } else {
                SuspensionReason::RevivalDeferred
            })
        );
    }
}

#[test]
fn detached_rollback_restores_absent_accumulated_cost_and_burns_epoch() {
    let temp = temp_store();
    let registry = Arc::new(FakeRegistry::default());
    let lifecycle = lifecycle(&temp.store, &registry, default_settings());
    let mut prior = seed_record(
        &temp.store,
        Seed {
            task_id: "st_0000a001",
            residency_state: Some(ResidencyState::PersistedOnly),
            ..Default::default()
        },
    );
    prior.run_stats = Some(crate::state::TaskRunStats {
        cost_usd: Some(0.25),
        ..Default::default()
    });
    temp.store.replace(&prior).expect("prior");
    let mut speculative = prior.clone();
    speculative.status = TaskStatus::Running;
    speculative.notification.run_epoch += 1;
    speculative.prior_runs_cost_usd = Some(0.25);
    speculative.run_stats = None;
    speculative.residency_state = ResidencyState::Resident;
    speculative.host_pid = Some(lifecycle.context().host_pid);
    temp.store.replace(&speculative).expect("speculative");
    assert_eq!(
        lifecycle
            .rollback_detached_revival(&prior)
            .expect("rollback"),
        crate::lifecycle::revive_detached::DetachedRevivalRollbackResult::RolledBack
    );
    let fresh = temp
        .store
        .load("st_0000a001")
        .expect("load")
        .expect("record");
    assert_eq!(fresh.prior_runs_cost_usd, None);
    assert_eq!(fresh.run_stats, prior.run_stats);
    assert_eq!(fresh.burnt_epoch, Some(1.0));
}

#[test]
fn explicit_stopped_owner_cancels_pending_children_before_parking() {
    let temp = temp_store();
    let registry = Arc::new(FakeRegistry::default());
    let lifecycle = lifecycle(&temp.store, &registry, default_settings());
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_0000a002",
            status: Some(TaskStatus::Cancelled),
            ..Default::default()
        },
    );
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_0000a001",
            status: Some(TaskStatus::Pending),
            host_pid: Some(lifecycle.context().host_pid),
            ..Default::default()
        },
    );
    let result = lifecycle
        .suspend_on_session_shutdown(&SuspendInput {
            parent_session_id: "parent-1".into(),
            own_task_id: Some("st_0000a002".into()),
            reason: "quit".into(),
        })
        .expect("shutdown");
    assert_eq!(result.stopped_owner.as_deref(), Some("st_0000a002"));
    assert_eq!(result.disposed, 1);
    assert_eq!(result.suspended_pending, 0);
    assert_eq!(
        temp.store
            .load("st_0000a001")
            .expect("load")
            .expect("st_0000a001")
            .status,
        TaskStatus::Cancelled
    );
}

#[test]
fn quit_stamps_pending_without_changing_its_status() {
    for reason in ["quit", "reload"] {
        let temp = temp_store();
        let registry = Arc::new(FakeRegistry::default());
        let lifecycle = lifecycle(&temp.store, &registry, default_settings());
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_0000a001",
                status: Some(TaskStatus::Pending),
                host_pid: Some(lifecycle.context().host_pid),
                ..Default::default()
            },
        );
        lifecycle
            .suspend_on_session_shutdown(&SuspendInput {
                parent_session_id: "parent-1".into(),
                own_task_id: None,
                reason: reason.into(),
            })
            .expect("shutdown");
        let fresh = temp
            .store
            .load("st_0000a001")
            .expect("load")
            .expect("st_0000a001");
        assert_eq!(fresh.status, TaskStatus::Pending);
        assert_eq!(
            fresh.suspension_reason,
            (reason == "quit").then_some(SuspensionReason::SessionQuit)
        );
    }
}

#[test]
fn suspended_expiry_clears_queue_and_reports_the_same_count() {
    let temp = temp_store();
    let registry = Arc::new(FakeRegistry::default());
    let lifecycle = lifecycle(&temp.store, &registry, default_settings());
    let mut record = seed_record(
        &temp.store,
        Seed {
            task_id: "st_0000a001",
            status: Some(TaskStatus::Running),
            residency_state: Some(ResidencyState::PersistedOnly),
            ..Default::default()
        },
    );
    record.pending_steering = Some(vec![
        crate::state::PendingSteeringEntry::new(
            "one".into(),
            "first".into(),
            crate::state::DeliverAs::Steer,
        ),
        crate::state::PendingSteeringEntry::new(
            "two".into(),
            "late".into(),
            crate::state::DeliverAs::Steer,
        ),
    ]);
    temp.store.replace(&record).expect("replace");
    crate::lifecycle::suspended_expiry::expire_suspended_child(
        lifecycle.context(),
        &record,
        &|| true,
    )
    .expect("expire");
    let fresh = temp
        .store
        .load("st_0000a001")
        .expect("load")
        .expect("st_0000a001");
    assert_eq!(fresh.pending_steering, None);
    assert_eq!(fresh.status, TaskStatus::Error);
    assert!(read_events(&temp.store, "st_0000a001").contains(&"suspended_unresumable".into()));
}

#[test]
fn paused_recovery_renews_deadline_instead_of_ending_child() {
    let temp = temp_store();
    let registry = Arc::new(FakeRegistry::default());
    let now = Arc::new(std::sync::atomic::AtomicI64::new(5_000_000));
    let mut deps = LifecycleDeps::new(temp.store.clone(), registry, default_settings());
    let clock = now.clone();
    deps.now = Some(Arc::new(move || {
        clock.load(std::sync::atomic::Ordering::SeqCst)
    }));
    deps.respawn = Some(Arc::new(|_, _| crate::lifecycle::RespawnResult::Failed {
        disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
        code: crate::lifecycle::port::RespawnFailureCode::RunPaused,
        reason: "run_paused".into(),
    }));
    deps.reattach = Some(Arc::new(|_, _| panic!("paused child must not attach")));
    let context = crate::lifecycle::resolve_context(deps);
    context
        .live_parents
        .lock()
        .expect("parents")
        .insert("parent-1".into());
    let mut record = seed_record(
        &temp.store,
        Seed {
            task_id: "st_0000a001",
            status: Some(TaskStatus::Running),
            residency_state: Some(ResidencyState::PersistedOnly),
            ..Default::default()
        },
    );
    record.spawn_spec = Some(crate::state::TaskSpawnSpec::V1(crate::state::SpawnSpecV1 {
        cwd: "/tmp".into(),
        prompt: "resume".into(),
        instructions: None,
        member_scoped_tool_names: None,
        isolation: None,
    }));
    temp.store.replace(&record).expect("replace");
    let recovery = crate::lifecycle::live_parent_recovery::LiveParentRecovery::default();
    recovery.scan(&context).expect("first scan");
    now.fetch_add(300_000, std::sync::atomic::Ordering::SeqCst);
    recovery.scan(&context).expect("deadline scan");
    let fresh = temp
        .store
        .load("st_0000a001")
        .expect("load")
        .expect("child");
    assert_eq!(fresh.status, TaskStatus::Running);
    assert_eq!(fresh.failure_kind, None);
    assert_eq!(fresh.revival_deferred_reason.as_deref(), Some("run_paused"));
    assert_eq!(fresh.recovery_deadline_at, Some(5_600_000.0));
}

#[test]
fn dead_legacy_process_under_paused_run_is_deferred_never_lost() {
    let temp = temp_store();
    let registry = Arc::new(FakeRegistry::default());
    let mut deps = LifecycleDeps::new(temp.store.clone(), registry, default_settings());
    deps.signaller = Some(FakeSignaller::alive([]));
    deps.respawn = Some(Arc::new(|_, _| crate::lifecycle::RespawnResult::Failed {
        disposition: crate::lifecycle::port::RespawnDisposition::Retryable,
        code: crate::lifecycle::port::RespawnFailureCode::RunPaused,
        reason: "run_paused".into(),
    }));
    deps.reattach = Some(Arc::new(|_, _| panic!("paused child must not attach")));
    let lifecycle = create_task_lifecycle(deps);
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_0000a001",
            status: Some(TaskStatus::Running),
            execution_mode: Some("process"),
            pid: Some(900),
            ..Default::default()
        },
    );
    let directory =
        crate::lifecycle::reconcile::child_session_dir(temp.store.state_dir(), "st_0000a001");
    std::fs::create_dir_all(&directory).expect("sessions");
    std::fs::write(directory.join("child.jsonl"), "{}\n").expect("transcript");
    let result = lifecycle
        .reconcile_on_session_start(None)
        .expect("reconcile");
    let outcome = result
        .outcomes
        .iter()
        .find(|outcome| outcome.task_id == "st_0000a001")
        .expect("outcome");
    assert_eq!(
        outcome.kind,
        crate::lifecycle::ReconcileOutcomeKind::Deferred
    );
    assert_eq!(outcome.reason.as_deref(), Some("run_paused"));
    assert_eq!(
        temp.store
            .load("st_0000a001")
            .expect("load")
            .expect("child")
            .status,
        TaskStatus::Running
    );
    assert!(!read_events(&temp.store, "st_0000a001").contains(&"reconcile_lost".into()));
}
