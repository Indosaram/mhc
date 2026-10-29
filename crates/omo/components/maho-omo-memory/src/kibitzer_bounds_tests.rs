use super::*;

#[test]
fn given_a_fresh_wake_when_armed_then_the_deadline_is_now_plus_the_quiet_period() {
    assert_eq!(WakeBounds::new(1_000).deadline_at(1_000), 91_000);
}

#[test]
fn given_a_long_running_wake_when_a_steer_re_arms_then_the_ceiling_clamps_it() {
    let bounds = WakeBounds::new(0);
    assert_eq!(bounds.deadline_at(260_000), 300_000);
    assert_eq!(bounds.total_deadline_at(), 300_000);
}

#[test]
fn given_the_ceiling_when_reached_then_it_is_reported() {
    let bounds = WakeBounds::with_limits(100, 50, 200);
    assert!(!bounds.at_ceiling(299));
    assert!(bounds.at_ceiling(300));
    assert_eq!(bounds.deadline_at(100), 150);
    assert_eq!(bounds.deadline_at(280), 300);
}
