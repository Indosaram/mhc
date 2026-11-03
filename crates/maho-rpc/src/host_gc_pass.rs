//! The budgeted gc pass `ensure_host` schedules after it has returned a host (senpi `rpc/host-gc-pass.ts`): the
//! same per-endpoint removal as `host gc` (`gc_endpoint` - each endpoint's own ensure lock with its 2 s wait, the
//! three-part evidence, siblings/socket/directory in that order), run without an operator so dead endpoint
//! records stop accumulating under an agent directory.
//!
//! It is never on the awaited start path: [`schedule_opportunistic_host_gc`] returns at once and the pass runs on
//! a spawned task, so a first child never waits on it, and a short-lived CLI process simply exits under it (the
//! marker is written only after a COMPLETED pass, so the next long-lived process picks the work up). At most one
//! pass per agent directory every `min_interval_ms`, stopped by a time and a count budget. It never considers -
//! and so never locks - the endpoint the ensure was for, and never lowers the evidence bar. The stale-engine stop
//! `host gc` performs is not ported (`host_gc.rs`), so this pass never ends a process.
//!
//! Fairness: endpoints are walked in sorted directory order starting AFTER the marker's `cursor` (the last one
//! the previous pass examined), wrapping around, so one uncollectable endpoint costs at most one slot per pass
//! and every endpoint is reached within ceil(total / max_endpoints) passes. An endpoint that stayed is skipped
//! until its `skip_until` (10 min, doubling to 24 h), so a stuck one stops consuming budget altogether.
use crate::host_gc::{Judged, gc_endpoint};
use crate::host_gc_pass_marker::{
    GcPassStoppedBy, HostGcPassMarker, HostGcSkipEntry, next_skip, read_host_gc_pass_marker,
    write_host_gc_pass_marker,
};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// senpi `OPPORTUNISTIC_GC_BUDGET`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OpportunisticGcBudget {
    pub budget_ms: f64,
    pub max_endpoints: usize,
    pub min_interval_ms: f64,
}
pub const OPPORTUNISTIC_GC_BUDGET: OpportunisticGcBudget = OpportunisticGcBudget {
    budget_ms: 1_500.,
    max_endpoints: 32,
    min_interval_ms: 300_000.,
};

pub type GcClock = Arc<dyn Fn() -> f64 + Send + Sync>;

#[derive(Clone)]
pub struct OpportunisticGcOptions {
    pub agent_dir: PathBuf,
    /// The endpoint the ensure was for: never judged, never locked.
    pub exclude: String,
    pub budget_ms: Option<f64>,
    pub max_endpoints: Option<usize>,
    pub min_interval_ms: Option<f64>,
    pub now: Option<GcClock>,
}
impl OpportunisticGcOptions {
    pub fn new(agent_dir: impl Into<PathBuf>, exclude: impl Into<String>) -> Self {
        Self {
            agent_dir: agent_dir.into(),
            exclude: exclude.into(),
            budget_ms: None,
            max_endpoints: None,
            min_interval_ms: None,
            now: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OpportunisticGcOutcome {
    NotRun,
    Ran(HostGcPassMarker),
}

struct NamedEndpoint {
    socket: String,
    dir: PathBuf,
    name: String,
}

fn wall_clock_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0., |elapsed| elapsed.as_millis() as f64)
}

fn platform() -> &'static str {
    if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    }
}

pub async fn gc_host_endpoints_opportunistically(
    options: &OpportunisticGcOptions,
) -> std::io::Result<OpportunisticGcOutcome> {
    let now = options
        .now
        .clone()
        .unwrap_or_else(|| Arc::new(wall_clock_ms));
    let budget_ms = options
        .budget_ms
        .unwrap_or(OPPORTUNISTIC_GC_BUDGET.budget_ms);
    let max_endpoints = options
        .max_endpoints
        .unwrap_or(OPPORTUNISTIC_GC_BUDGET.max_endpoints);
    let min_interval_ms = options
        .min_interval_ms
        .unwrap_or(OPPORTUNISTIC_GC_BUDGET.min_interval_ms);
    let previous = read_host_gc_pass_marker(&options.agent_dir)?;
    let started_at = now();
    if previous
        .as_ref()
        .is_some_and(|previous| started_at - previous.completed_at < min_interval_ms)
    {
        return Ok(OpportunisticGcOutcome::NotRun);
    }
    #[cfg(unix)]
    {
        let dir = options.agent_dir.clone();
        let budget = crate::rpc_public_key::KeyGcBudget {
            deadline: Some(
                std::time::Instant::now()
                    + std::time::Duration::from_millis(budget_ms.max(0.) as u64),
            ),
            max_records: Some(max_endpoints),
        };
        tokio::task::spawn_blocking(move || {
            crate::rpc_public_key::gc_rpc_public_keys(&dir, budget)
        })
        .await
        .map_err(std::io::Error::other)??;
    }
    let candidates = gc_candidates(options);
    let present: HashSet<&str> = candidates
        .iter()
        .map(|endpoint| endpoint.name.as_str())
        .collect();
    let mut skip: BTreeMap<String, HostGcSkipEntry> = previous
        .as_ref()
        .map(|previous| {
            previous
                .skip
                .iter()
                .filter(|(name, _)| present.contains(name.as_str()))
                .map(|(name, entry)| (name.clone(), *entry))
                .collect()
        })
        .unwrap_or_default();
    let mut cursor = previous.and_then(|previous| previous.cursor);
    let mut examined = 0usize;
    let mut removed = 0usize;
    let mut stopped_by = GcPassStoppedBy::Exhausted;
    for endpoint in rotate_after(&candidates, cursor.as_deref()) {
        if skip
            .get(&endpoint.name)
            .is_some_and(|entry| entry.skip_until > now())
        {
            continue;
        }
        if examined >= max_endpoints {
            stopped_by = GcPassStoppedBy::Count;
            break;
        }
        if now() - started_at >= budget_ms {
            stopped_by = GcPassStoppedBy::Time;
            break;
        }
        examined += 1;
        cursor = Some(endpoint.name.clone());
        // senpi host-gc-pass.ts: `gcEndpoint(socket, dir, { staleEngine: {} })`.
        match gc_endpoint(&endpoint.socket, &endpoint.dir, Some(&Default::default())).await {
            Ok(Judged::Removed { .. }) => {
                removed += 1;
                skip.remove(&endpoint.name);
            }
            Ok(Judged::Kept(_)) | Ok(Judged::Stopped { .. }) | Err(_) => {
                let next = next_skip(skip.get(&endpoint.name), now());
                skip.insert(endpoint.name.clone(), next);
            }
        }
    }
    let marker = HostGcPassMarker {
        completed_at: now(),
        cursor,
        examined: examined as f64,
        removed: removed as f64,
        stopped_by,
        skip,
    };
    write_host_gc_pass_marker(&options.agent_dir, &marker)?;
    Ok(OpportunisticGcOutcome::Ran(marker))
}

type PassRegistry = Mutex<HashMap<PathBuf, tokio::sync::watch::Receiver<bool>>>;
fn passes_running() -> &'static PassRegistry {
    static RUNNING: std::sync::OnceLock<PassRegistry> = std::sync::OnceLock::new();
    RUNNING.get_or_init(Default::default)
}

/// Starts a pass off the caller's path; at most one at a time per agent directory in this process. Needs a tokio
/// runtime; a caller without one schedules nothing.
pub fn schedule_opportunistic_host_gc(options: OpportunisticGcOptions) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let mut running = passes_running()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if running.contains_key(&options.agent_dir) {
        return;
    }
    let (done, settled) = tokio::sync::watch::channel(false);
    running.insert(options.agent_dir.clone(), settled);
    drop(running);
    runtime.spawn(async move {
        if let Err(error) = gc_host_endpoints_opportunistically(&options).await {
            record_pass_failure(&options.agent_dir, &error);
        }
        passes_running()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&options.agent_dir);
        let _ = done.send(true);
    });
}

/// Whether a pass this process scheduled for `agent_dir` has not finished yet.
#[cfg(test)]
pub(crate) fn opportunistic_host_gc_pending(agent_dir: &Path) -> bool {
    passes_running()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains_key(agent_dir)
}

/// Resolves once the pass this process scheduled for `agent_dir` (if any) has finished; it never fails.
pub async fn settled_opportunistic_host_gc(agent_dir: &Path) {
    let settled = passes_running()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(agent_dir)
        .cloned();
    if let Some(mut settled) = settled {
        let _ = settled.wait_for(|done| *done).await;
    }
}

fn gc_candidates(options: &OpportunisticGcOptions) -> Vec<NamedEndpoint> {
    let target_name =
        crate::host_daemon_paths::create_host_daemon_paths(&options.exclude, &options.agent_dir)
            .dir
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
    crate::host_endpoints::list_host_endpoints(&options.agent_dir)
        .into_iter()
        .filter_map(|endpoint| {
            let socket = endpoint.socket?;
            let name = endpoint.dir.file_name()?.to_string_lossy().into_owned();
            if Some(&name) == target_name.as_ref()
                || crate::host_endpoint_names::same_endpoint(&socket, &options.exclude, platform())
            {
                return None;
            }
            Some(NamedEndpoint {
                socket,
                dir: endpoint.dir,
                name,
            })
        })
        .collect()
}

fn rotate_after<'a>(
    endpoints: &'a [NamedEndpoint],
    cursor: Option<&str>,
) -> Vec<&'a NamedEndpoint> {
    let start = cursor
        .and_then(|cursor| {
            endpoints
                .iter()
                .position(|endpoint| endpoint.name.as_str() > cursor)
        })
        .unwrap_or(0);
    endpoints[start..]
        .iter()
        .chain(endpoints[..start].iter())
        .collect()
}

fn record_pass_failure(agent_dir: &Path, error: &std::io::Error) {
    let line = format!(
        "{} gc pass failed: {error}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
    );
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = options
        .open(agent_dir.join("rpc-host-daemon").join("gc-pass.log"))
        .and_then(|mut file| std::io::Write::write_all(&mut file, line.as_bytes()));
    match written {
        // The agent directory itself is gone: nothing is left to collect or to report to.
        Err(log_error) if log_error.kind() == std::io::ErrorKind::NotFound => {}
        Err(log_error) => {
            eprintln!("host gc pass failed and could not be recorded: {log_error}");
        }
        Ok(()) => {}
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::host_gc_pass_marker::GC_SKIP_INITIAL_BACKOFF_MS;

    fn dead_endpoint(agent_dir: &Path, socket: &str) -> PathBuf {
        let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
        crate::host_daemon_paths::create_daemon_directories(&paths)
            .expect("create daemon directories");
        std::fs::write(
            &paths.endpoint_file,
            serde_json::json!({ "socket": socket }).to_string(),
        )
        .expect("write dead endpoint file");
        paths.dir
    }
    fn dead_endpoints(agent_dir: &Path, count: usize) -> Vec<(String, PathBuf)> {
        let mut endpoints = (0..count)
            .map(|index| {
                let socket = agent_dir
                    .join("d")
                    .join(format!("e{index}.sock"))
                    .to_string_lossy()
                    .into_owned();
                let dir = dead_endpoint(agent_dir, &socket);
                (socket, dir)
            })
            .collect::<Vec<_>>();
        endpoints.sort_by(|left, right| left.1.file_name().cmp(&right.1.file_name()));
        endpoints
    }
    fn fixed(instant: f64) -> Option<GcClock> {
        Some(Arc::new(move || instant))
    }
    fn options(agent_dir: &Path, exclude: &str, now: Option<GcClock>) -> OpportunisticGcOptions {
        OpportunisticGcOptions {
            now,
            ..OpportunisticGcOptions::new(agent_dir, exclude)
        }
    }
    fn expire_interval(agent_dir: &Path) {
        let marker = read_host_gc_pass_marker(agent_dir)
            .expect("read host gc pass marker")
            .expect("host gc pass marker present");
        write_host_gc_pass_marker(
            agent_dir,
            &HostGcPassMarker {
                completed_at: marker.completed_at - 301_000.,
                ..marker
            },
        )
        .expect("write host gc pass marker");
    }
    fn ran(outcome: OpportunisticGcOutcome) -> HostGcPassMarker {
        match outcome {
            OpportunisticGcOutcome::Ran(marker) => marker,
            OpportunisticGcOutcome::NotRun => panic!("the pass did not run"),
        }
    }

    #[tokio::test]
    async fn runs_at_most_once_per_interval_a_second_pass_inside_it_does_no_work() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path();
        let target = agent_dir.join("t.sock").to_string_lossy().into_owned();
        let endpoints = dead_endpoints(agent_dir, 2);
        let first =
            ran(
                gc_host_endpoints_opportunistically(&options(agent_dir, &target, fixed(1e12)))
                    .await
                    .unwrap(),
            );
        assert_eq!(first.removed, 2.);
        dead_endpoint(agent_dir, &endpoints[0].0);
        assert_eq!(
            gc_host_endpoints_opportunistically(&options(agent_dir, &target, fixed(1e12)))
                .await
                .unwrap(),
            OpportunisticGcOutcome::NotRun
        );
        assert!(endpoints[0].1.exists());
    }

    #[tokio::test]
    async fn never_judges_the_target_endpoint_even_when_it_is_dead() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path();
        let endpoints = dead_endpoints(agent_dir, 1);
        let marker = ran(gc_host_endpoints_opportunistically(&options(
            agent_dir,
            &endpoints[0].0,
            fixed(1e12),
        ))
        .await
        .unwrap());
        assert_eq!(marker.examined, 0.);
        assert!(endpoints[0].1.exists());
    }

    #[tokio::test]
    async fn rotates_past_an_uncollectable_head_without_examining_it_again() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path();
        let target = agent_dir.join("t.sock").to_string_lossy().into_owned();
        let endpoints = dead_endpoints(agent_dir, 5);
        let (head, dead) = endpoints.split_first().unwrap();
        // The socket answers: the head is kept as `reachable`.
        std::fs::create_dir_all(Path::new(&head.0).parent().unwrap()).unwrap();
        let _listener = std::os::unix::net::UnixListener::bind(&head.0).unwrap();
        let head_name = head.1.file_name().unwrap().to_string_lossy().into_owned();
        let first = ran(
            gc_host_endpoints_opportunistically(&OpportunisticGcOptions {
                max_endpoints: Some(1),
                ..options(agent_dir, &target, fixed(1e12))
            })
            .await
            .unwrap(),
        );
        assert_eq!(first.cursor.as_deref(), Some(head_name.as_str()));
        assert_eq!((first.examined, first.removed), (1., 0.));
        assert_eq!(
            first.skip[&head_name].backoff_ms,
            GC_SKIP_INITIAL_BACKOFF_MS
        );
        let mut removed = 0.;
        for _ in 0..2 {
            expire_interval(agent_dir);
            let marker = ran(
                gc_host_endpoints_opportunistically(&OpportunisticGcOptions {
                    max_endpoints: Some(2),
                    ..options(agent_dir, &target, fixed(1e12))
                })
                .await
                .unwrap(),
            );
            assert!(marker.examined <= 2.);
            removed += marker.removed;
        }
        assert_eq!(removed, 4.);
        assert!(dead.iter().all(|(_, dir)| !dir.exists()));
        let marker = read_host_gc_pass_marker(agent_dir).unwrap().unwrap();
        // Examined a second time, the backoff would have doubled.
        assert_eq!(
            marker.skip[&head_name].backoff_ms,
            GC_SKIP_INITIAL_BACKOFF_MS
        );
        assert!(head.1.exists());
    }

    #[tokio::test]
    async fn leaves_endpoints_for_a_later_pass_when_scanning_consumes_the_time_budget() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path();
        let target = agent_dir.join("t.sock").to_string_lossy().into_owned();
        let endpoints = dead_endpoints(agent_dir, 2);
        let started_at = 1e12;
        let clock = Arc::new(Mutex::new(started_at));
        let ticking: GcClock = {
            let clock = Arc::clone(&clock);
            Arc::new(move || {
                let mut clock = clock.lock().unwrap();
                let value = *clock;
                *clock = started_at + OPPORTUNISTIC_GC_BUDGET.budget_ms;
                value
            })
        };
        let spent =
            ran(
                gc_host_endpoints_opportunistically(&options(agent_dir, &target, Some(ticking)))
                    .await
                    .unwrap(),
            );
        assert_eq!((spent.examined, spent.removed), (0., 0.));
        assert_eq!(spent.stopped_by, GcPassStoppedBy::Time);
        assert!(endpoints.iter().all(|(_, dir)| dir.exists()));
        expire_interval(agent_dir);
        let later = started_at + OPPORTUNISTIC_GC_BUDGET.budget_ms;
        let next =
            ran(
                gc_host_endpoints_opportunistically(&options(agent_dir, &target, fixed(later)))
                    .await
                    .unwrap(),
            );
        assert_eq!(next.removed, 2.);
        assert!(endpoints.iter().all(|(_, dir)| !dir.exists()));
    }

    #[tokio::test]
    async fn a_scheduled_pass_settles_and_writes_its_marker() {
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path();
        let endpoints = dead_endpoints(agent_dir, 1);
        let target = agent_dir.join("t.sock").to_string_lossy().into_owned();
        schedule_opportunistic_host_gc(OpportunisticGcOptions::new(agent_dir, target.clone()));
        // A second schedule while the first runs is a no-op.
        schedule_opportunistic_host_gc(OpportunisticGcOptions::new(agent_dir, target));
        settled_opportunistic_host_gc(agent_dir).await;
        settled_opportunistic_host_gc(agent_dir).await;
        let marker = read_host_gc_pass_marker(agent_dir).unwrap().unwrap();
        assert_eq!(marker.removed, 1.);
        assert!(!endpoints[0].1.exists());
    }
}
