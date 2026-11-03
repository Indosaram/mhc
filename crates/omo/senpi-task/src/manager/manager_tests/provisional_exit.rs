use super::fakes::{HarnessOptions, WAIT, base_spec, lock, make_manager, started, wait_terminal};
use crate::runners::in_process::child_handle::RunnerExit;
use crate::runners::{RunnerFailure, RunnerFailureKind, RunnerOutcome};
use crate::state::TaskStatus;
use std::sync::{Arc, Mutex, atomic::{AtomicI64, Ordering}, mpsc::channel};

#[test]
fn process_exit_stays_provisional_until_the_full_confirmation_window() {
    let now = Arc::new(AtomicI64::new(1_000));
    let clock = now.clone();
    let (entered_tx, entered) = channel();
    let (advance, advances) = channel::<i64>();
    let advances = Arc::new(Mutex::new(advances));
    let tick = now.clone();
    let h = make_manager(HarnessOptions {
        customize: Some(Box::new(move |options| {
            options.now = Some(Arc::new(move || clock.load(Ordering::SeqCst)));
            options.exit_confirmation_wait = Some(Arc::new(move |remaining| {
                entered_tx.send(remaining).expect("confirmation listener");
                let delta = lock(&advances).recv_timeout(WAIT).expect("advance confirmation clock");
                tick.fetch_add(delta, Ordering::SeqCst);
            }));
        })),
        ..Default::default()
    });
    *lock(&h.in_process.child_pid) = Some(4242);
    let task = started(h.manager.start(&base_spec()));
    let handle = h.in_process.handle(&task.task_id).expect("started handle");
    handle.settle(RunnerOutcome::Error {
        failure: RunnerFailure::new(RunnerFailureKind::ChildTurnFailed, "child crashed")
            .with_exit(RunnerExit { kind: "crash", code: Some(1), signal: None }),
        killed: false,
    });
    assert_eq!(entered.recv_timeout(WAIT).expect("first confirmation"), 2_000);
    let pending = h.store.load(&task.task_id).expect("load").expect("record");
    assert_eq!(pending.status, TaskStatus::Running);
    assert_eq!(pending.provisional_exit.as_ref().expect("provisional").code, Some(1));
    let output_deps = crate::tools::output::types::TaskOutputDeps {
        manager: Arc::new(h.manager.clone()),
        state_dir: h.store.state_dir().to_string_lossy().into_owned(),
        transcript_reader: None,
        resolve_caller_session_id: None,
        now: Some(Arc::new(|| 1_000)),
    };
    for mode in [crate::tools::output::output::TaskOutputMode::Status,
        crate::tools::output::output::TaskOutputMode::Tail,
        crate::tools::output::output::TaskOutputMode::Full] {
        let result = crate::tools::output::output::run_task_output(&output_deps,
            &crate::tools::output::output::TaskOutputInput {
                task_id: Some(task.task_id.clone()), mode: Some(mode), ..Default::default()
            }, Some("parent-1")).expect("provisional output");
        let snapshot = match &result.details {
            crate::tools::output::types::TaskOutputDetails::Status { snapshot }
                | crate::tools::output::types::TaskOutputDetails::Transcript { snapshot, .. } => snapshot,
            other => panic!("unexpected output: {other:?}"),
        };
        assert_eq!(snapshot.status, TaskStatus::Running);
        assert_eq!(snapshot.error_message, None);
        let theme = ProvisionalTheme(std::cell::RefCell::new(Vec::new()));
        crate::tools::output::renderers::render_task_output_result(&result, &Default::default(), &theme);
        assert!(!theme.0.borrow().contains(&crate::tools::render::ThemeColor::Error));
    }
    advance.send(500).expect("early wake");
    assert_eq!(entered.recv_timeout(WAIT).expect("rearmed confirmation"), 1_500);
    assert_eq!(h.store.load(&task.task_id).expect("load").expect("record").status, TaskStatus::Running);
    advance.send(1_500).expect("confirm");
    let terminal = wait_terminal(&h.manager, &task.task_id);
    assert_eq!(terminal.status, TaskStatus::Error);
    assert_eq!(terminal.provisional_exit, None);
    assert_eq!(terminal.terminal_at.as_deref(), Some(crate::shared::iso_from_ms(3_000).as_str()));
    assert!(terminal.error_message.is_some());
}

struct ProvisionalTheme(std::cell::RefCell<Vec<crate::tools::render::ThemeColor>>);

#[cfg(unix)]
#[test]
fn failed_provisional_write_still_confirms_and_settles_the_terminal_waiter() {
    use std::os::unix::fs::PermissionsExt;
    struct RestorePermissions(std::path::PathBuf, std::fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            std::fs::set_permissions(&self.0, self.1.clone()).expect("restore task directory");
        }
    }
    let now = Arc::new(AtomicI64::new(1_000));
    let clock = now.clone();
    let (entered_tx, entered) = channel();
    let (release, released) = channel();
    let released = Mutex::new(released);
    let tick = now.clone();
    let h = make_manager(HarnessOptions {
        customize: Some(Box::new(move |options| {
            options.now = Some(Arc::new(move || clock.load(Ordering::SeqCst)));
            options.exit_confirmation_wait = Some(Arc::new(move |remaining| {
                entered_tx.send(remaining).expect("confirmation listener");
                lock(&released).recv_timeout(WAIT).expect("release confirmation");
                tick.fetch_add(2_000, Ordering::SeqCst);
            }));
        })),
        ..Default::default()
    });
    *lock(&h.in_process.child_pid) = Some(4242);
    let task = started(h.manager.start(&base_spec()));
    let handle = h.in_process.handle(&task.task_id).expect("started handle");
    let directory = h.store.state_dir().join("tasks");
    let permissions = std::fs::metadata(&directory).expect("tasks metadata").permissions();
    let restore = RestorePermissions(directory.clone(), permissions);
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).expect("refuse writes");
    handle.settle(RunnerOutcome::Error {
        failure: RunnerFailure::new(RunnerFailureKind::ChildTurnFailed, "exit-fixture")
            .with_exit(RunnerExit { kind: "crashed", code: Some(1), signal: None }),
        killed: false,
    });
    assert_eq!(entered.recv_timeout(WAIT).expect("failed write still waits"), 2_000);
    let pending = h.store.load(&task.task_id).expect("load").expect("record");
    assert_eq!(pending.status, TaskStatus::Running);
    assert_eq!(pending.provisional_exit, None, "fixture must reject the provisional write");
    drop(restore);
    release.send(()).expect("confirm after restoring storage");
    assert_eq!(wait_terminal(&h.manager, &task.task_id).status, TaskStatus::Error);
}

impl crate::tools::render::RendererTheme for ProvisionalTheme {
    fn fg(&self, color: crate::tools::render::ThemeColor, text: &str) -> String {
        self.0.borrow_mut().push(color);
        text.to_owned()
    }
    fn italic(&self, text: &str) -> String { text.to_owned() }
}

#[test]
fn replaced_epoch_is_not_settled_by_the_old_provisional_exit() {
    // Given a process exit whose confirmation is held at the clock boundary.
    let (entered_tx, entered) = channel();
    let (release, released) = channel();
    let released = Mutex::new(released);
    let h = make_manager(HarnessOptions {
        customize: Some(Box::new(move |options| {
            options.now = Some(Arc::new(|| 1_000));
            options.exit_confirmation_wait = Some(Arc::new(move |_| {
                entered_tx.send(()).expect("confirmation listener");
                lock(&released).recv_timeout(WAIT).expect("release confirmation");
            }));
        })),
        ..Default::default()
    });
    *lock(&h.in_process.child_pid) = Some(4242);
    let task = started(h.manager.start(&base_spec()));
    let handle = h.in_process.handle(&task.task_id).expect("started handle");
    handle.settle(RunnerOutcome::Error {
        failure: RunnerFailure::new(RunnerFailureKind::ChildTurnFailed, "exit-fixture")
            .with_exit(RunnerExit { kind: "crash", code: Some(1), signal: None }),
        killed: false,
    });
    entered.recv_timeout(WAIT).expect("confirmation entered");
    // When another run takes ownership before the old watcher wakes.
    let replacement = h.store.mutate(&task.task_id, |fresh| {
        let mut next = fresh.clone();
        next.notification.run_epoch += 1;
        next.provisional_exit = None;
        next
    }).expect("replace epoch").expect("replacement record");
    release.send(()).expect("wake stale watcher");
    crate::manager::outcome::test_barrier::wait_until_consumed(Arc::as_ptr(&handle) as usize);
    // Then the stale outcome cannot write a terminal state into the replacement.
    let fresh = h.store.load(&task.task_id).expect("load").expect("record");
    assert_eq!(fresh.notification.run_epoch, replacement.notification.run_epoch);
    assert_eq!(fresh.status, TaskStatus::Running);
    assert_eq!(fresh.provisional_exit, None);
}
