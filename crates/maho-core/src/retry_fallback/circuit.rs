//! Port of senpi packages/coding-agent/src/core/retry-fallback/circuit.ts.
//!
//! Fallback-chain circuit breaker. An entry that fails out of a configured chain opens a circuit keyed by
//! its base selector (`provider/id`); while it is open, chain resolution skips the entry. One breaker is
//! shared by every session in the process that uses the same agent directory. The breaker is clock-free:
//! every call receives `now`.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use maho_ai::types::UsageLimitScope;
use maho_ai::utils::retry_hint::parse_retry_after_ms_marker;

use super::circuit_probes::is_health_exhaustion_failure;
use super::usage_limit::usage_limit_scope_text;
use crate::credential_pool::reset_time::{ResetKind, usage_limit_reset};

pub const DEFAULT_CIRCUIT_COOLDOWN_MS: f64 = 60_000.0;
pub const DEFAULT_CIRCUIT_MAX_COOLDOWN_MS: f64 = 30.0 * 60_000.0;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct FallbackCircuitSettings {
    /// First cooldown after an entry fails out of a chain; doubles per consecutive open. 0 disables the breaker.
    pub circuit_cooldown_ms: Option<f64>,
    pub circuit_max_cooldown_ms: Option<f64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ResolvedFallbackCircuitSettings {
    pub cooldown_ms: f64,
    pub max_cooldown_ms: f64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CircuitOpenRequest {
    pub now: f64,
    pub cooldown_ms: f64,
    pub max_cooldown_ms: f64,
    pub retry_after_ms: Option<f64>,
    /// The reset window a usage limit named for itself; when present it alone sets the open.
    pub reset_window_ms: Option<f64>,
    /// The failure is a verdict for every session: the entry is spent (billing, quota, an account limit).
    pub exhausted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeToken {
    pub selector: String,
    pub owner: String,
    pub generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitAdmission {
    Closed,
    Open,
    Probe { token: ProbeToken },
}

impl CircuitAdmission {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Closed => "closed",
            Self::Open => "open",
            Self::Probe { .. } => "probe",
        }
    }
}

#[derive(Debug, Clone)]
struct Probe {
    owner: String,
    generation: u64,
}

#[derive(Debug, Clone)]
struct SelectorCircuit {
    open_until: f64,
    retry_floor_until: f64,
    consecutive_opens: u32,
    cooldown_ms: f64,
    max_cooldown_ms: f64,
    probe: Option<Probe>,
    exhausted: bool,
}

fn non_negative_ms(value: Option<f64>, fallback: f64) -> f64 {
    value
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(fallback)
}

pub fn resolve_fallback_circuit_settings(
    settings: Option<FallbackCircuitSettings>,
) -> ResolvedFallbackCircuitSettings {
    let settings = settings.unwrap_or_default();
    let cooldown_ms = non_negative_ms(settings.circuit_cooldown_ms, DEFAULT_CIRCUIT_COOLDOWN_MS);
    let max_cooldown_ms = non_negative_ms(
        settings.circuit_max_cooldown_ms,
        DEFAULT_CIRCUIT_MAX_COOLDOWN_MS,
    );
    ResolvedFallbackCircuitSettings {
        cooldown_ms,
        max_cooldown_ms: cooldown_ms.max(max_cooldown_ms),
    }
}

fn positive(value: Option<f64>) -> Option<f64> {
    value.filter(|value| value.is_finite() && *value > 0.0)
}

#[derive(Debug, Default)]
pub struct FallbackCircuitBreaker {
    circuits: HashMap<String, SelectorCircuit>,
    generations: u64,
}

impl FallbackCircuitBreaker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn size(&self) -> usize {
        self.circuits.len()
    }

    pub fn open(&mut self, selector: &str, request: CircuitOpenRequest) -> f64 {
        let previous = self.circuits.get(selector);
        // A failure while the circuit is still open belongs to the same open episode: it takes no backoff
        // step. Only a failure once the open has elapsed, a failed half-open probe, advances it (senpi#3467).
        let same_episode = previous.is_some_and(|previous| request.now < previous.open_until);
        let consecutive_opens =
            previous.map_or(0, |previous| previous.consecutive_opens) + u32::from(!same_episode);
        let cooldown_ms = match previous.filter(|_| same_episode) {
            Some(previous) => previous.cooldown_ms,
            None => (request.cooldown_ms * 2f64.powi(consecutive_opens as i32 - 1))
                .min(request.max_cooldown_ms),
        };
        let hinted =
            positive(request.retry_after_ms).map_or(0.0, |retry_after| request.now + retry_after);
        let retry_floor_until = hinted.max(
            previous
                .filter(|previous| previous.retry_floor_until > request.now)
                .map_or(0.0, |p| p.retry_floor_until),
        );
        // A provider Retry-After lengthens the wait up to the ceiling, never past it. A usage limit's own reset
        // window sets the open, up to the ceiling; it never cuts a Retry-After wait still running (senpi#3314).
        let hinted_until = retry_floor_until.min(request.now + request.max_cooldown_ms);
        let open_until = match positive(request.reset_window_ms) {
            Some(reset_window) => {
                (request.now + reset_window.min(request.max_cooldown_ms)).max(hinted_until)
            }
            None => match previous.filter(|_| same_episode) {
                Some(previous) => previous.open_until.max(hinted_until),
                None => (request.now + cooldown_ms).max(hinted_until),
            },
        };
        let exhausted = request.exhausted
            || (same_episode && previous.is_some_and(|previous| previous.exhausted));
        self.circuits.insert(
            selector.to_owned(),
            SelectorCircuit {
                open_until,
                retry_floor_until,
                consecutive_opens,
                cooldown_ms,
                max_cooldown_ms: request.max_cooldown_ms,
                probe: None,
                exhausted,
            },
        );
        open_until
    }

    pub fn is_open(&self, selector: &str, now: f64, owner: &str) -> bool {
        let Some(circuit) = self.circuits.get(selector) else {
            return false;
        };
        if now < circuit.open_until {
            return true;
        }
        circuit
            .probe
            .as_ref()
            .is_some_and(|probe| probe.owner != owner)
    }

    /// Atomic admission: a closed selector passes, a cooling one or one whose probe another requester holds
    /// is refused, and a half-open one hands `owner` its single probe under a fresh generation. The probe
    /// stays exclusive until its token is released or the circuit settles.
    pub fn admit(&mut self, selector: &str, now: f64, owner: &str) -> CircuitAdmission {
        let Some(circuit) = self.circuits.get_mut(selector) else {
            return CircuitAdmission::Closed;
        };
        if now < circuit.open_until {
            return CircuitAdmission::Open;
        }
        if circuit
            .probe
            .as_ref()
            .is_some_and(|probe| probe.owner != owner)
        {
            return CircuitAdmission::Open;
        }
        let generation = match &circuit.probe {
            Some(probe) => probe.generation,
            None => {
                self.generations += 1;
                circuit.probe = Some(Probe {
                    owner: owner.to_owned(),
                    generation: self.generations,
                });
                self.generations
            }
        };
        CircuitAdmission::Probe {
            token: ProbeToken {
                selector: selector.to_owned(),
                owner: owner.to_owned(),
                generation,
            },
        }
    }

    pub fn release(&mut self, token: &ProbeToken) {
        if let Some(circuit) = self.circuits.get_mut(&token.selector)
            && circuit.probe.as_ref().is_some_and(|probe| {
                probe.owner == token.owner && probe.generation == token.generation
            })
        {
            circuit.probe = None;
        }
    }

    pub fn release_owners_with_prefix(&mut self, prefix: &str) {
        for circuit in self.circuits.values_mut() {
            if circuit
                .probe
                .as_ref()
                .is_some_and(|probe| probe.owner.starts_with(prefix))
            {
                circuit.probe = None;
            }
        }
    }

    pub fn close(&mut self, selector: &str) {
        self.circuits.remove(selector);
    }

    pub fn has(&self, selector: &str) -> bool {
        self.circuits.contains_key(selector)
    }

    pub fn is_exhausted(&self, selector: &str) -> bool {
        self.circuits
            .get(selector)
            .is_some_and(|circuit| circuit.exhausted)
    }

    pub fn sweep(&mut self, now: f64) {
        self.circuits.retain(|_, circuit| {
            circuit.probe.is_some() || now < circuit.open_until + circuit.max_cooldown_ms
        });
    }
}

/// A session's requests are admitted per lane: the foreground turn is one lane, each background probe-back
/// its own (`probe-back:<n>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitLane {
    Turn,
    ProbeBack(u64),
}

fn lane_owner(session: &str, lane: CircuitLane) -> String {
    match lane {
        CircuitLane::Turn => format!("{session}:turn"),
        CircuitLane::ProbeBack(index) => format!("{session}:probe-back:{index}"),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct CircuitFailureInput<'a> {
    pub retry_after_ms: Option<f64>,
    pub error_message: Option<&'a str>,
}

pub type SharedBreaker = Arc<Mutex<FallbackCircuitBreaker>>;

/// Logger hook: `(event, selector, duration_ms, retry_after_ms, reset_window_ms)`.
pub type CircuitLogger = Arc<dyn Fn(&str, &str, f64, Option<f64>, Option<f64>) + Send + Sync>;

pub struct FallbackCircuitAccessDeps {
    pub breaker: SharedBreaker,
    pub owner: Arc<dyn Fn() -> String + Send + Sync>,
    pub now: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// Wall clock for parsing reset times; the open deadline itself stays on `now`.
    pub wall_now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    pub settings: Arc<dyn Fn() -> ResolvedFallbackCircuitSettings + Send + Sync>,
    pub logger: Option<CircuitLogger>,
}

/// One session's access to the shared breaker (upstream `createFallbackCircuitAccess`).
pub struct FallbackCircuitAccess {
    deps: FallbackCircuitAccessDeps,
}

fn lock(breaker: &SharedBreaker) -> MutexGuard<'_, FallbackCircuitBreaker> {
    breaker
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl FallbackCircuitAccess {
    pub fn new(deps: FallbackCircuitAccessDeps) -> Self {
        Self { deps }
    }

    fn enabled(&self) -> bool {
        (self.deps.settings)().cooldown_ms > 0.0
    }

    pub fn note_failure(&self, selector: &str, failure: CircuitFailureInput<'_>) {
        if !self.enabled() {
            return;
        }
        let ResolvedFallbackCircuitSettings {
            cooldown_ms,
            max_cooldown_ms,
        } = (self.deps.settings)();
        let now = (self.deps.now)();
        let message = failure.error_message;
        let retry_after_ms = failure.retry_after_ms.or_else(|| {
            message
                .and_then(parse_retry_after_ms_marker)
                .map(|ms| ms as f64)
        });
        // A usage limit sets the window only with an absolute reset or a relative one at least the base
        // cooldown; "retry in 7s" on a spent quota is pacing, not a recovery time. Reset times are wall-clock.
        let scope = usage_limit_scope_text(message);
        let wall_now = self
            .deps
            .wall_now
            .as_ref()
            .map_or(now, |wall_now| wall_now());
        let reset = message
            .filter(|_| scope.is_some())
            .and_then(|message| usage_limit_reset(message, wall_now as i64));
        let reset_window_ms = reset
            .filter(|reset| reset.kind == ResetKind::Absolute || reset.ms as f64 >= cooldown_ms)
            .map(|reset| reset.ms as f64);
        let exhausted = message.is_some_and(|message| {
            is_health_exhaustion_failure(Some(message)) || scope == Some(UsageLimitScope::Account)
        });
        let open_until = {
            let mut breaker = lock(&self.deps.breaker);
            breaker.sweep(now);
            breaker.open(
                selector,
                CircuitOpenRequest {
                    now,
                    cooldown_ms,
                    max_cooldown_ms,
                    retry_after_ms,
                    reset_window_ms,
                    exhausted,
                },
            )
        };
        if let Some(logger) = &self.deps.logger {
            logger(
                "circuit_opened",
                selector,
                open_until - now,
                retry_after_ms,
                reset_window_ms,
            );
        }
    }

    /// Whether the breaker holds any state for the selector, i.e. it, not a session cooldown, governs recovery.
    pub fn governs(&self, selector: &str) -> bool {
        self.enabled() && lock(&self.deps.breaker).has(selector)
    }

    /// Whether the open circuit records a verdict every session should act on: the entry is spent.
    pub fn is_exhausted(&self, selector: &str) -> bool {
        self.enabled() && lock(&self.deps.breaker).is_exhausted(selector)
    }

    pub fn is_open(&self, selector: &str, lane: CircuitLane) -> bool {
        self.enabled()
            && lock(&self.deps.breaker).is_open(
                selector,
                (self.deps.now)(),
                &lane_owner(&(self.deps.owner)(), lane),
            )
    }

    pub fn admit(&self, selector: &str, lane: CircuitLane) -> CircuitAdmission {
        if !self.enabled() {
            return CircuitAdmission::Closed;
        }
        lock(&self.deps.breaker).admit(
            selector,
            (self.deps.now)(),
            &lane_owner(&(self.deps.owner)(), lane),
        )
    }

    pub fn release(&self, token: &ProbeToken) {
        lock(&self.deps.breaker).release(token);
    }

    pub fn release_all(&self) {
        lock(&self.deps.breaker).release_owners_with_prefix(&format!("{}:", (self.deps.owner)()));
    }

    pub fn close(&self, selector: &str) {
        lock(&self.deps.breaker).close(selector);
    }
}

struct BreakerEntry {
    breaker: SharedBreaker,
    owners: usize,
}

static BREAKERS_BY_AGENT_DIR: LazyLock<Mutex<HashMap<PathBuf, BreakerEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// `path.resolve`: absolute, with `.` and `..` folded lexically.
fn resolve(agent_dir: &str) -> PathBuf {
    let path = Path::new(agent_dir);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut resolved = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            other => resolved.push(other.as_os_str()),
        }
    }
    resolved
}

/// An entry leaves the registry only when no live session holds it and it has no circuit left to share.
fn evict_unheld_empty_breakers(registry: &mut HashMap<PathBuf, BreakerEntry>) {
    registry.retain(|_, entry| entry.owners > 0 || lock(&entry.breaker).size() > 0);
}

fn with_entry<T>(agent_dir: &str, use_entry: impl FnOnce(&mut BreakerEntry) -> T) -> T {
    let key = resolve(agent_dir);
    let mut registry = BREAKERS_BY_AGENT_DIR
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !registry.contains_key(&key) {
        evict_unheld_empty_breakers(&mut registry);
    }
    let entry = registry.entry(key).or_insert_with(|| BreakerEntry {
        breaker: Arc::new(Mutex::new(FallbackCircuitBreaker::new())),
        owners: 0,
    });
    use_entry(entry)
}

pub fn fallback_circuits_for(agent_dir: &str) -> SharedBreaker {
    with_entry(agent_dir, |entry| entry.breaker.clone())
}

/// Holds the agent dir's breaker for one session; `release` is idempotent.
pub struct FallbackCircuitsLease {
    pub breaker: SharedBreaker,
    key: PathBuf,
    released: bool,
}

impl FallbackCircuitsLease {
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let mut registry = BREAKERS_BY_AGENT_DIR
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = registry.get_mut(&self.key)
            && Arc::ptr_eq(&entry.breaker, &self.breaker)
        {
            entry.owners = entry.owners.saturating_sub(1);
        }
        evict_unheld_empty_breakers(&mut registry);
    }
}

pub fn acquire_fallback_circuits(agent_dir: &str) -> FallbackCircuitsLease {
    let key = resolve(agent_dir);
    let breaker = with_entry(agent_dir, |entry| {
        entry.owners += 1;
        entry.breaker.clone()
    });
    FallbackCircuitsLease {
        breaker,
        key,
        released: false,
    }
}

/// Milliseconds on a monotonic clock anchored at the wall clock of the first call.
pub fn monotonic_now() -> f64 {
    static ORIGIN: LazyLock<(std::time::Instant, f64)> = LazyLock::new(|| {
        let wall = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_secs_f64() * 1000.0)
            .unwrap_or(0.0);
        (std::time::Instant::now(), wall)
    });
    ORIGIN.1 + ORIGIN.0.elapsed().as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    //! senpi `test/suite/retry-fallback-circuit-breaker.test.ts` and `retry-fallback-circuit-reset-window.test.ts`.
    use super::*;

    const HEAD: &str = "provider-a/model-x";

    fn window(now: f64) -> CircuitOpenRequest {
        CircuitOpenRequest {
            now,
            cooldown_ms: 1_000.0,
            max_cooldown_ms: 3_000.0,
            ..Default::default()
        }
    }

    fn ceiling(now: f64) -> CircuitOpenRequest {
        CircuitOpenRequest {
            now,
            cooldown_ms: 1_000.0,
            max_cooldown_ms: 1_800_000.0,
            ..Default::default()
        }
    }

    #[test]
    fn doubles_the_cooldown_on_each_failed_half_open_probe_up_to_the_ceiling() {
        let mut breaker = FallbackCircuitBreaker::new();
        assert_eq!(breaker.open(HEAD, window(0.0)), 1_000.0);
        assert_eq!(breaker.open(HEAD, window(1_000.0)), 3_000.0);
        assert_eq!(breaker.open(HEAD, window(3_000.0)), 6_000.0);
        assert_eq!(breaker.open(HEAD, window(6_000.0)), 9_000.0);
    }

    #[test]
    fn takes_one_backoff_step_per_open_episode() {
        let mut breaker = FallbackCircuitBreaker::new();
        let defaults = |now| CircuitOpenRequest {
            now,
            cooldown_ms: 60_000.0,
            max_cooldown_ms: 1_800_000.0,
            ..Default::default()
        };
        let ends: Vec<f64> = (0..6)
            .map(|second| breaker.open(HEAD, defaults(second as f64 * 1_000.0)))
            .collect();
        assert_eq!(ends, vec![60_000.0; 6]);
        assert_eq!(breaker.open(HEAD, defaults(60_000.0)), 180_000.0);
    }

    #[test]
    fn resets_the_escalation_when_the_circuit_closes() {
        let mut breaker = FallbackCircuitBreaker::new();
        breaker.open(HEAD, window(0.0));
        breaker.open(HEAD, window(0.0));
        breaker.close(HEAD);
        assert!(!breaker.is_open(HEAD, 0.0, "a"));
        assert_eq!(breaker.open(HEAD, window(0.0)), 1_000.0);
    }

    #[test]
    fn keeps_the_circuit_open_until_a_provider_retry_after_longer_than_the_cooldown() {
        let mut breaker = FallbackCircuitBreaker::new();
        assert_eq!(
            breaker.open(
                HEAD,
                CircuitOpenRequest {
                    retry_after_ms: Some(600_000.0),
                    ..ceiling(100.0)
                }
            ),
            600_100.0
        );
        assert!(breaker.is_open(HEAD, 600_099.0, "a"));
        assert!(!breaker.is_open(HEAD, 600_100.0, "a"));
    }

    #[test]
    fn keeps_an_outstanding_retry_after_deadline_when_a_later_failure_carries_no_hint() {
        let mut breaker = FallbackCircuitBreaker::new();
        assert_eq!(
            breaker.open(
                HEAD,
                CircuitOpenRequest {
                    retry_after_ms: Some(600_000.0),
                    ..ceiling(0.0)
                }
            ),
            600_000.0
        );
        assert_eq!(breaker.open(HEAD, ceiling(1_000.0)), 600_000.0);
        assert!(breaker.is_open(HEAD, 599_999.0, "a"));
    }

    #[test]
    fn bounds_a_retry_after_longer_than_the_ceiling_so_one_half_open_probe_is_admitted() {
        let mut breaker = FallbackCircuitBreaker::new();
        assert_eq!(
            breaker.open(
                HEAD,
                CircuitOpenRequest {
                    retry_after_ms: Some(600_000.0),
                    ..window(100.0)
                }
            ),
            3_100.0
        );
        assert_eq!(breaker.admit(HEAD, 3_099.0, "a"), CircuitAdmission::Open);
        assert_eq!(breaker.admit(HEAD, 3_100.0, "a").kind(), "probe");
        assert_eq!(breaker.admit(HEAD, 3_100.0, "b"), CircuitAdmission::Open);
    }

    #[test]
    fn admits_exactly_one_probe_owner_and_keeps_it_exclusive_until_it_settles() {
        let mut breaker = FallbackCircuitBreaker::new();
        breaker.open(HEAD, window(0.0));
        assert_eq!(breaker.admit(HEAD, 999.0, "a"), CircuitAdmission::Open);
        let first = breaker.admit(HEAD, 1_000.0, "a");
        assert_eq!(first.kind(), "probe");
        assert_eq!(breaker.admit(HEAD, 1_000.0, "b"), CircuitAdmission::Open);
        assert_eq!(
            breaker.admit(HEAD, 1_000_000.0, "b"),
            CircuitAdmission::Open
        );
        assert_eq!(breaker.admit(HEAD, 1_000_000.0, "a"), first);
    }

    #[test]
    fn releases_a_probe_only_for_the_token_that_holds_it() {
        let mut breaker = FallbackCircuitBreaker::new();
        breaker.open(HEAD, window(0.0));
        let CircuitAdmission::Probe { token } = breaker.admit(HEAD, 1_000.0, "a") else {
            panic!("expected a probe")
        };
        breaker.release(&ProbeToken {
            owner: "b".into(),
            ..token.clone()
        });
        assert!(breaker.is_open(HEAD, 1_000.0, "b"));
        breaker.open(HEAD, window(1_000.0));
        breaker.release(&token);
        assert_eq!(breaker.admit(HEAD, 3_000.0, "b").kind(), "probe");
        breaker.release_owners_with_prefix("b");
        assert_eq!(breaker.admit(HEAD, 3_000.0, "c").kind(), "probe");
    }

    #[test]
    fn sweeps_only_circuits_idle_for_a_full_ceiling_window_after_they_half_opened() {
        let mut breaker = FallbackCircuitBreaker::new();
        breaker.open(HEAD, window(0.0));
        breaker.open("provider-b/model-y", window(0.0));
        breaker.admit("provider-b/model-y", 1_000.0, "a");
        breaker.sweep(3_999.0);
        assert_eq!(breaker.size(), 2);
        breaker.sweep(4_000.0);
        assert_eq!(breaker.size(), 1);
        assert!(breaker.is_open("provider-b/model-y", 4_000.0, "b"));
    }

    fn temp_agent_dir(name: &str) -> String {
        std::env::temp_dir()
            .join(format!("{name}-{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned()
    }

    #[test]
    fn keeps_a_held_breakers_identity_while_other_agent_directories_come_and_go() {
        let agent_dir = temp_agent_dir("circuit-breaker-held");
        let mut lease = acquire_fallback_circuits(&agent_dir);
        fallback_circuits_for(&format!("{agent_dir}-unrelated"));
        assert!(Arc::ptr_eq(
            &fallback_circuits_for(&agent_dir),
            &lease.breaker
        ));
        lease.release();
        lease.release();
        fallback_circuits_for(&format!("{agent_dir}-another"));
        assert!(!Arc::ptr_eq(
            &fallback_circuits_for(&agent_dir),
            &lease.breaker
        ));
    }

    #[test]
    fn keeps_an_unheld_breaker_that_still_has_circuits_to_share() {
        let agent_dir = temp_agent_dir("circuit-breaker-unheld-open");
        let mut lease = acquire_fallback_circuits(&agent_dir);
        lock(&lease.breaker).open(HEAD, window(0.0));
        lease.release();
        fallback_circuits_for(&format!("{agent_dir}-unrelated"));
        assert!(Arc::ptr_eq(
            &fallback_circuits_for(&agent_dir),
            &lease.breaker
        ));
    }

    #[test]
    fn shares_one_breaker_per_resolved_agent_directory() {
        let agent_dir = temp_agent_dir("circuit-breaker-agent");
        let nested = Path::new(&agent_dir)
            .join("sub")
            .join("..")
            .to_string_lossy()
            .into_owned();
        // Hold it so a concurrent test's eviction cannot replace it between the two lookups.
        let _lease = acquire_fallback_circuits(&agent_dir);
        assert!(Arc::ptr_eq(
            &fallback_circuits_for(&agent_dir),
            &fallback_circuits_for(&nested)
        ));
        assert!(!Arc::ptr_eq(
            &fallback_circuits_for(&agent_dir),
            &fallback_circuits_for(&format!("{agent_dir}-other"))
        ));
    }

    #[test]
    fn defaults_to_60s_doubling_up_to_30_minutes_and_rejects_malformed_values() {
        let defaults = ResolvedFallbackCircuitSettings {
            cooldown_ms: DEFAULT_CIRCUIT_COOLDOWN_MS,
            max_cooldown_ms: DEFAULT_CIRCUIT_MAX_COOLDOWN_MS,
        };
        assert_eq!(resolve_fallback_circuit_settings(None), defaults);
        assert_eq!(DEFAULT_CIRCUIT_COOLDOWN_MS, 60_000.0);
        assert_eq!(DEFAULT_CIRCUIT_MAX_COOLDOWN_MS, 1_800_000.0);
        let malformed = FallbackCircuitSettings {
            circuit_cooldown_ms: Some(-1.0),
            circuit_max_cooldown_ms: Some(f64::NAN),
        };
        assert_eq!(resolve_fallback_circuit_settings(Some(malformed)), defaults);
        let inverted = FallbackCircuitSettings {
            circuit_cooldown_ms: Some(5_000.0),
            circuit_max_cooldown_ms: Some(1_000.0),
        };
        assert_eq!(
            resolve_fallback_circuit_settings(Some(inverted)),
            ResolvedFallbackCircuitSettings {
                cooldown_ms: 5_000.0,
                max_cooldown_ms: 5_000.0
            }
        );
    }

    type Clock = Arc<Mutex<(f64, f64)>>;

    fn access(
        breaker: SharedBreaker,
        cooldown_ms: f64,
        max_cooldown_ms: f64,
        clock: Clock,
    ) -> FallbackCircuitAccess {
        let wall = clock.clone();
        FallbackCircuitAccess::new(FallbackCircuitAccessDeps {
            breaker,
            owner: Arc::new(|| "session-a".to_owned()),
            now: Arc::new(move || clock.lock().map(|c| c.0).unwrap_or(0.0)),
            wall_now: Some(Arc::new(move || wall.lock().map(|c| c.1).unwrap_or(0.0))),
            settings: Arc::new(move || ResolvedFallbackCircuitSettings {
                cooldown_ms,
                max_cooldown_ms,
            }),
            logger: None,
        })
    }

    fn shared() -> SharedBreaker {
        Arc::new(Mutex::new(FallbackCircuitBreaker::new()))
    }

    #[test]
    fn opens_circuits_with_the_sessions_clock_and_settings() {
        let circuits = access(shared(), 1_000.0, 1_000.0, Arc::new(Mutex::new((0.0, 0.0))));
        circuits.note_failure(HEAD, CircuitFailureInput::default());
        assert!(circuits.is_open(HEAD, CircuitLane::Turn));
    }

    #[test]
    fn reads_a_retry_after_carried_as_the_error_message_marker() {
        let breaker = shared();
        let circuits = access(
            breaker.clone(),
            1_000.0,
            1_800_000.0,
            Arc::new(Mutex::new((0.0, 0.0))),
        );
        let failure = CircuitFailureInput {
            error_message: Some("503: Provider unavailable (retry-after-ms: 90000)"),
            ..Default::default()
        };
        circuits.note_failure(HEAD, failure);
        assert!(lock(&breaker).is_open(HEAD, 89_999.0, "session-b"));
        assert!(!lock(&breaker).is_open(HEAD, 90_000.0, "session-b"));
    }

    #[test]
    fn neither_opens_nor_honours_circuits_when_circuit_cooldown_ms_is_0() {
        let breaker = shared();
        lock(&breaker).open(HEAD, window(0.0));
        let circuits = access(breaker.clone(), 0.0, 0.0, Arc::new(Mutex::new((0.0, 0.0))));
        circuits.note_failure("provider-b/model-y", CircuitFailureInput::default());
        assert!(!circuits.is_open(HEAD, CircuitLane::Turn));
        assert_eq!(
            circuits.admit(HEAD, CircuitLane::Turn),
            CircuitAdmission::Closed
        );
        assert!(!lock(&breaker).is_open("provider-b/model-y", 0.0, "session-a"));
    }

    const SELECTOR: &str = "faux/faux-1";
    const MINUTE: f64 = 60_000.0;
    // Date.UTC(2026, 9, 14, 16, 47)
    const WALL_EPOCH: f64 = 1_791_996_420_000.0;

    fn reset_clock() -> (Clock, FallbackCircuitAccess) {
        let clock: Clock = Arc::new(Mutex::new((0.0, WALL_EPOCH)));
        let circuits = access(shared(), MINUTE, 30.0 * MINUTE, clock.clone());
        (clock, circuits)
    }

    fn advance(clock: &Clock, mono: f64, wall: f64) {
        if let Ok(mut clock) = clock.lock() {
            clock.0 += mono;
            clock.1 += wall;
        }
    }

    fn session_limit_reset_in(minutes: u32) -> String {
        format!(
            "Devin stream error resource_exhausted: Reached free model rate limit. Upgrade to Max for higher limits, or switch to a different model. Your limit will reset in {minutes} minutes."
        )
    }

    fn message(text: &str) -> CircuitFailureInput<'_> {
        CircuitFailureInput {
            error_message: Some(text),
            retry_after_ms: None,
        }
    }

    #[test]
    fn honors_a_usage_limits_two_minute_reset() {
        let (clock, circuit) = reset_clock();
        let text = session_limit_reset_in(2);
        for _ in 0..3 {
            circuit.note_failure(SELECTOR, message(&text));
        }
        advance(&clock, 2.5 * MINUTE, 2.5 * MINUTE);
        assert!(!circuit.is_open(SELECTOR, CircuitLane::Turn));
    }

    #[test]
    fn stays_open_until_a_twenty_minute_reset() {
        let (clock, circuit) = reset_clock();
        circuit.note_failure(SELECTOR, message(&session_limit_reset_in(20)));
        advance(&clock, 10.0 * MINUTE, 0.0);
        assert!(circuit.is_open(SELECTOR, CircuitLane::Turn));
        advance(&clock, 11.0 * MINUTE, 0.0);
        assert!(!circuit.is_open(SELECTOR, CircuitLane::Turn));
    }

    #[test]
    fn keeps_the_doubling_cooldown_floor_for_short_hints() {
        let failures: Vec<CircuitFailureInput<'static>> = vec![
            CircuitFailureInput {
                retry_after_ms: Some(1_000.0),
                error_message: None,
            },
            message("429 Too Many Requests: please try again in 2s"),
            message("overloaded, retry in 1 second"),
            message(
                "429 You exceeded your current quota, please check your plan and billing details. Please retry in 7s.",
            ),
            message("You've hit your usage limit. Try again in 3 seconds."),
            message("You've hit your usage limit. Please retry in 2s."),
            message("You've hit your usage limit. Your limit will reset in 30 seconds."),
        ];
        for failure in failures {
            let (clock, circuit) = reset_clock();
            let mut closes_after = Vec::new();
            for _ in 0..3 {
                let opened_at = clock.lock().map(|c| c.0).unwrap_or(0.0);
                circuit.note_failure(SELECTOR, failure);
                while circuit.is_open(SELECTOR, CircuitLane::Turn) {
                    advance(&clock, 1_000.0, 0.0);
                }
                closes_after.push(clock.lock().map(|c| c.0).unwrap_or(0.0) - opened_at);
            }
            assert_eq!(
                closes_after,
                vec![MINUTE, 2.0 * MINUTE, 4.0 * MINUTE],
                "{failure:?}"
            );
        }
    }

    #[test]
    fn keeps_a_running_retry_after_wait_over_a_shorter_reset() {
        let (clock, circuit) = reset_clock();
        circuit.note_failure(
            SELECTOR,
            CircuitFailureInput {
                retry_after_ms: Some(10.0 * MINUTE),
                error_message: None,
            },
        );
        circuit.note_failure(SELECTOR, message(&session_limit_reset_in(2)));
        advance(&clock, 5.0 * MINUTE, 0.0);
        assert!(circuit.is_open(SELECTOR, CircuitLane::Turn));
    }

    #[test]
    fn ends_the_open_at_an_absolute_reset_thirty_seconds_away() {
        let (clock, circuit) = reset_clock();
        let text = format!(
            r#"429 {{"error":{{"type":"usage_limit_reached","resets_at":{}}}}}"#,
            WALL_EPOCH / 1000.0 + 30.0
        );
        circuit.note_failure(SELECTOR, message(&text));
        advance(&clock, 31_000.0, 0.0);
        assert!(!circuit.is_open(SELECTOR, CircuitLane::Turn));
    }

    #[test]
    fn follows_the_wall_clock_for_resets_at_when_the_monotonic_clock_drifted() {
        let clock: Clock = Arc::new(Mutex::new((5_000.0, WALL_EPOCH)));
        let circuit = access(shared(), MINUTE, 30.0 * MINUTE, clock.clone());
        let text = format!(
            r#"429 {{"error":{{"type":"usage_limit_reached","resets_at":{}}}}}"#,
            WALL_EPOCH / 1000.0 + 120.0
        );
        circuit.note_failure(SELECTOR, message(&text));
        advance(&clock, 100_000.0, 0.0);
        assert!(circuit.is_open(SELECTOR, CircuitLane::Turn));
        advance(&clock, 30_000.0, 0.0);
        assert!(!circuit.is_open(SELECTOR, CircuitLane::Turn));
    }
}
