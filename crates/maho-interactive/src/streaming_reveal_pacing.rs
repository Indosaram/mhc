//! Port of streaming-reveal-pacing.ts.
pub const INITIAL_BUFFER_MS: f64 = 80.;
pub const TARGET_BUFFER_MS: f64 = 140.;
pub const CATCHUP_WINDOW_MS: f64 = 267.;
pub const MIN_ARRIVAL_UNITS_PER_SEC: f64 = 45.;
pub const MAX_ARRIVAL_SAMPLE_MULTIPLIER: f64 = 4.;
pub const MAX_EXTRA_CATCHUP_UNITS_PER_SEC: f64 = 600.;
pub const ARRIVAL_RATE_ALPHA: f64 = 0.25;
pub const MIN_SMOOTH_FPS: f64 = 30.;
pub const MAX_SMOOTH_FPS: f64 = 120.;
pub const DEFAULT_SMOOTH_FPS: f64 = 60.;
pub fn update_arrival_rate(current: f64, appended: f64, elapsed: f64) -> f64 {
    if appended <= 0. || elapsed <= 0. {
        return current;
    }
    let maximum = MIN_ARRIVAL_UNITS_PER_SEC.max(current * MAX_ARRIVAL_SAMPLE_MULTIPLIER);
    let sample = (appended * 1000. / elapsed)
        .max(MIN_ARRIVAL_UNITS_PER_SEC)
        .min(maximum);
    current + ARRIVAL_RATE_ALPHA * (sample - current)
}
pub fn next_step(backlog: f64, dt_ms: f64, arrival_rate: f64) -> f64 {
    if backlog <= 0. {
        return 0.;
    }
    let dt = dt_ms.clamp(1., 100.);
    let base = arrival_rate.max(MIN_ARRIVAL_UNITS_PER_SEC);
    let target = base * TARGET_BUFFER_MS / 1000.;
    let correction = ((backlog - target) * 1000. / CATCHUP_WINDOW_MS)
        .clamp(-base, MAX_EXTRA_CATCHUP_UNITS_PER_SEC);
    backlog.min((base + correction).max(0.) * dt / 1000.)
}
pub fn smooth_fps(configured: f64) -> f64 {
    if configured.is_finite() {
        configured.clamp(MIN_SMOOTH_FPS, MAX_SMOOTH_FPS)
    } else {
        DEFAULT_SMOOTH_FPS
    }
}
