use senpi_task::runners::rpc::heartbeat::{HEARTBEAT_DEADLINE_MS, HeartbeatMachine, MissVerdict};

#[test]
fn successful_heartbeat_activity_resets_failures_and_extends_deadline() {
    let mut machine = HeartbeatMachine::default();
    assert_eq!(machine.miss(0), MissVerdict::Missed);
    let now = 10_000;
    machine.activity(now);
    assert_eq!(machine.failures(), 0);
    assert_eq!(
        machine.miss(now + 1_000),
        MissVerdict::Rearm(HEARTBEAT_DEADLINE_MS - 1_000)
    );
    assert_eq!(machine.failures(), 0);
}

#[test]
fn answered_probe_clears_prior_failures() {
    let mut machine = HeartbeatMachine::default();
    let now = HEARTBEAT_DEADLINE_MS;
    assert_eq!(machine.miss(now), MissVerdict::Missed);
    assert_eq!(machine.failures(), 1);
    machine.answered();
    assert_eq!(machine.failures(), 0);
}

#[test]
fn three_misses_declare_unresponsive_and_stop_machine() {
    let mut machine = HeartbeatMachine::default();
    assert_eq!(machine.miss(10_000), MissVerdict::Missed);
    assert_eq!(machine.miss(20_000), MissVerdict::Missed);
    assert_eq!(machine.miss(30_000), MissVerdict::Unresponsive);
    assert!(machine.stopped());
}

#[test]
fn probe_refusal_does_not_double_count_after_miss() {
    let mut machine = HeartbeatMachine::default();
    assert_eq!(machine.miss(10_000), MissVerdict::Missed);
    assert_eq!(machine.failures(), 1);
    assert_eq!(machine.refused(true), None);
    assert_eq!(machine.failures(), 1);
    assert_eq!(machine.refused(false), Some(MissVerdict::Missed));
    assert_eq!(machine.failures(), 2);
}
