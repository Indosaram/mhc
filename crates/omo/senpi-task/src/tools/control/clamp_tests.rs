//! `tools/control/clamp.test.ts`

use crate::tools::control::clamp::{WaitBounds, clamp_wait_timeout};
use pretty_assertions::assert_eq;

const BOUNDS: WaitBounds = WaitBounds {
    min_ms: 5000,
    default_ms: 60000,
    max_ms: 600000,
};

#[test]
fn given_below_min_timeout_when_clamped_then_rises_to_min() {
    // given
    let requested = 4999;
    // when
    let clamped = clamp_wait_timeout(Some(requested), &BOUNDS);
    // then
    assert_eq!(clamped, 5000);
}

#[test]
fn given_exactly_min_timeout_when_clamped_then_unchanged() {
    assert_eq!(clamp_wait_timeout(Some(5000), &BOUNDS), 5000);
}

#[test]
fn given_above_max_timeout_when_clamped_then_falls_to_max() {
    // given
    let requested = 999999;
    // when
    let clamped = clamp_wait_timeout(Some(requested), &BOUNDS);
    // then
    assert_eq!(clamped, 600000);
}

#[test]
fn given_exactly_max_timeout_when_clamped_then_unchanged() {
    assert_eq!(clamp_wait_timeout(Some(600000), &BOUNDS), 600000);
}

#[test]
fn given_an_in_range_timeout_when_clamped_then_passes_through() {
    assert_eq!(clamp_wait_timeout(Some(30000), &BOUNDS), 30000);
}

#[test]
fn given_no_timeout_when_clamped_then_uses_the_configured_default() {
    assert_eq!(clamp_wait_timeout(None, &BOUNDS), 60000);
}
