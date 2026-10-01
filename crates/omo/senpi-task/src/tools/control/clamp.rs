//! Port of `tools/control/clamp.ts`.

/// Mirrors `OmoTaskSettings["wait"]` (config min/default/max bounds in milliseconds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitBounds {
    pub min_ms: u64,
    pub default_ms: u64,
    pub max_ms: u64,
}

/// Codex wait contract (config min/default/max): an omitted timeout falls back to `default_ms`; any
/// supplied value is clamped into `[min_ms, max_ms]`. Boundary intent (defaults 5000/60000/600000):
/// 4999 -> 5000, 999999 -> 600000.
pub fn clamp_wait_timeout(requested: Option<u64>, bounds: &WaitBounds) -> u64 {
    let Some(requested) = requested else {
        return bounds.default_ms;
    };
    if requested < bounds.min_ms {
        return bounds.min_ms;
    }
    if requested > bounds.max_ms {
        return bounds.max_ms;
    }
    requested
}
