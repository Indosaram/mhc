//! Ports the state-machine cases of `rpc-host/handle-liveness.test.ts` and
//! `heartbeat-activity.test.ts` (host) against an explicit clock.

use std::sync::{Arc, Mutex};

use super::*;

const INTERVAL: i64 = 10_000;

/// Steps the machine to `until`, answering nothing: every probe stays pending.
fn advance_silent(
    machine: &mut HostHeartbeatMachine,
    from: i64,
    until: i64,
) -> (Vec<HeartbeatStep>, Vec<HostLivenessStatus>) {
    let mut steps = Vec::new();
    let mut changes = Vec::new();
    let mut now = from;
    while now <= until {
        let step = machine.tick(now, &mut changes);
        if step != HeartbeatStep::Idle {
            steps.push(step);
        }
        now += 1_000;
    }
    (steps, changes)
}

#[test]
fn missed_probes_publish_unresponsive_and_stay_single_flight() {
    let mut machine = HostHeartbeatMachine::new(INTERVAL, 0);
    let (steps, changes) = advance_silent(&mut machine, 0, 45_000);
    assert_eq!(
        steps
            .iter()
            .filter(|step| **step == HeartbeatStep::Probe)
            .count(),
        1,
        "one probe in flight: {steps:?}"
    );
    assert!(matches!(
        changes.as_slice(),
        [HostLivenessStatus::Unresponsive { .. }]
    ));
    assert!(!machine.stopped());
}

#[test]
fn a_good_probe_returns_to_running() {
    let mut machine = HostHeartbeatMachine::new(INTERVAL, 0);
    advance_silent(&mut machine, 0, 45_000);
    let mut changes = Vec::new();
    machine.answered(46_000, &mut changes);
    assert_eq!(changes, vec![HostLivenessStatus::Running]);
    assert_eq!(machine.status(), HostLivenessStatus::Running);
}

#[test]
fn an_overdue_session_goes_terminal_pending_and_retries_its_close() {
    let mut machine = HostHeartbeatMachine::new(INTERVAL, 0);
    let (steps, changes) = advance_silent(&mut machine, 0, 400_000);
    assert!(
        changes
            .iter()
            .any(|status| matches!(status, HostLivenessStatus::TerminalPending { .. }))
    );
    let closes = steps
        .iter()
        .filter(|step| **step == HeartbeatStep::Close)
        .count();
    assert!(
        closes > 1,
        "the close is retried until confirmed: {steps:?}"
    );
    // A late probe answer cannot undo the stop intent.
    let mut late = Vec::new();
    machine.answered(401_000, &mut late);
    assert!(late.is_empty());
    assert!(matches!(
        machine.status(),
        HostLivenessStatus::TerminalPending { .. }
    ));
}

#[test]
fn a_missing_session_answer_is_gone_without_a_close() {
    let mut machine = HostHeartbeatMachine::new(INTERVAL, 0);
    let mut changes = Vec::new();
    assert_eq!(machine.tick(10_000, &mut changes), HeartbeatStep::Probe);
    assert_eq!(
        machine.refused(10_500, true, &mut changes),
        HeartbeatStep::Gone
    );
    assert!(machine.stopped());
    assert_eq!(machine.tick(500_000, &mut changes), HeartbeatStep::Idle);
}

#[test]
fn live_events_reset_missed_probes_without_declaring_unresponsive() {
    let mut machine = HostHeartbeatMachine::new(INTERVAL, 0);
    let mut changes = Vec::new();
    let mut now = 0;
    for _ in 0..12 {
        now += 9_000;
        machine.tick(now, &mut changes);
        machine.activity(now, &mut changes);
    }
    assert!(changes.is_empty(), "{changes:?}");
    assert_eq!(machine.status(), HostLivenessStatus::Running);
    let (_, later) = advance_silent(&mut machine, now, now + 40_000);
    assert!(matches!(
        later.as_slice(),
        [HostLivenessStatus::Unresponsive { .. }]
    ));
}

#[test]
fn subscribers_see_changes_and_a_late_subscriber_gets_the_current_state() {
    let heartbeat = Arc::new(HostHeartbeat::new("st_1".into(), INTERVAL, 0));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let _subscription = heartbeat.subscribe(Arc::new(move |status| {
        sink.lock().expect("seen").push(status);
    }));
    heartbeat.step(|machine, changes| {
        let mut now = 0;
        while now <= 45_000 {
            machine.tick(now, changes);
            now += 1_000;
        }
    });
    assert!(matches!(
        seen.lock().expect("seen").as_slice(),
        [HostLivenessStatus::Unresponsive { .. }]
    ));
    let late = Arc::new(Mutex::new(Vec::new()));
    let late_sink = Arc::clone(&late);
    let _late = heartbeat.subscribe(Arc::new(move |status| {
        late_sink.lock().expect("late").push(status);
    }));
    assert_eq!(late.lock().expect("late").len(), 1);
}
