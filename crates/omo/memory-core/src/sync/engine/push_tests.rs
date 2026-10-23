use super::*;

#[test]
fn backoff_window_doubles_to_the_cap_with_upper_half_jitter() {
    assert_eq!(stale_backoff_ms(0, &|| 0.0), 125);
    assert_eq!(stale_backoff_ms(0, &|| 0.999_999), 249);
    assert_eq!(stale_backoff_ms(1, &|| 0.0), 250);
    assert_eq!(stale_backoff_ms(5, &|| 0.0), 4_000);
    assert_eq!(stale_backoff_ms(40, &|| 0.0), 4_000);
    assert_eq!(stale_backoff_ms(40, &|| 0.5), 6_000);
}

#[test]
fn system_random_is_a_unit_interval_draw() {
    let draw = system_random();
    assert!((0.0..1.0).contains(&draw));
}
