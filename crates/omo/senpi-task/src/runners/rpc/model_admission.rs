//! `runners/rpc/model-admission.ts`: reject a process child whose model the child profile cannot
//! resolve, before spawning it. The catalog probe runs through utils' process-tree executor.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, LazyLock, Mutex, PoisonError};

use regex::Regex;
use serde_json::json;

use crate::runners::rpc::process::RpcSpawnDescriptor;
use crate::runners::rpc::spawn::{RpcSpawnRuntime, build_rpc_model_catalog_spawn};
use crate::runners::types::RpcRunnerSpec;
use crate::runners::{RunnerFailure, RunnerFailureKind};

const PROBE_TIMEOUT_MS: u64 = 20_000;
const MAX_OUTPUT_BYTES: usize = 2 * 1024 * 1024;
/// A successful catalog is cached only briefly: a login or settings edit changes which models
/// resolve, and nothing in this process observes that.
pub const MODEL_CATALOG_CACHE_TTL_MS: i64 = 120_000;

static ANSI_ESCAPE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1B\[[0-?]*[ -/]*[@-~]").unwrap_or_else(|error| panic!("ansi regex: {error}"))
});

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelCatalogProbeResult {
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
}

pub type ModelCatalogProbe =
    Arc<dyn Fn(&RpcSpawnDescriptor) -> ModelCatalogProbeResult + Send + Sync>;
pub type CatalogSpawnBuilder = Arc<dyn Fn(&RpcRunnerSpec) -> RpcSpawnDescriptor + Send + Sync>;
pub type RpcModelAdmission = Arc<dyn Fn(&RpcRunnerSpec) -> Result<(), RunnerFailure> + Send + Sync>;

#[derive(Clone, Default)]
pub struct RpcModelAdmissionOptions {
    pub build_spawn: Option<CatalogSpawnBuilder>,
    pub probe: Option<ModelCatalogProbe>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

/// Parse `senpi --list-models` output into exact `provider/model` identities.
pub fn parse_model_catalog(output: &str) -> BTreeSet<String> {
    let mut models = BTreeSet::new();
    let cleaned = ANSI_ESCAPE.replace_all(output, "");
    for raw_line in cleaned.lines() {
        let mut columns = raw_line.split_whitespace();
        let Some(provider) = columns.next() else {
            continue;
        };
        if provider == "provider" {
            continue;
        }
        match columns.next() {
            Some(model) if model != "model" => {
                models.insert(format!("{provider}/{model}"));
            }
            Some(_) => {}
            None if provider.contains('/') => {
                models.insert(provider.to_string());
            }
            None => {}
        }
    }
    models
}

/// Run the catalog probe detached in its own process group (never through a shell); a timeout
/// result is returned only after the tree is terminated.
pub fn probe_model_catalog(
    descriptor: &RpcSpawnDescriptor,
    timeout_ms: Option<u64>,
) -> ModelCatalogProbeResult {
    let result = utils::process_tree::run_process_with_tree_timeout(
        &utils::process_tree::ProcessTreeRunOptions {
            command: descriptor.command.clone(),
            args: descriptor.args.clone(),
            cwd: PathBuf::from(&descriptor.cwd),
            env: descriptor.env.clone().into_iter().collect(),
            max_buffer: MAX_OUTPUT_BYTES,
            timeout_ms: timeout_ms.unwrap_or(PROBE_TIMEOUT_MS),
            termination_grace_ms: None,
            termination_wait_ms: None,
            on_termination_report: None,
        },
    );
    ModelCatalogProbeResult {
        code: (!result.timed_out && result.signal.is_none()).then_some(result.exit_code),
        stdout: result.stdout,
        stderr: result.stderr,
        timed_out: result.timed_out,
    }
}

fn profile_key(descriptor: &RpcSpawnDescriptor) -> String {
    let env = |name: &str| descriptor.env.get(name).cloned();
    json!([
        descriptor.command,
        descriptor.args,
        descriptor.cwd,
        env("MAHO_CODING_AGENT_DIR"),
        env("SENPI_CODING_AGENT_DIR"),
        env("PI_CODING_AGENT_DIR"),
        env("HOME"),
        env("USERPROFILE"),
        env("XDG_CONFIG_HOME"),
    ])
    .to_string()
}

fn admission_failure(model: &str, message: &str) -> RunnerFailure {
    RunnerFailure::new(
        RunnerFailureKind::ModelUnavailable,
        format!("process model admission failed for {model}: {message}"),
    )
}

fn trailing_chars(value: &str, limit: usize) -> String {
    let count = value.chars().count();
    value.chars().skip(count.saturating_sub(limit)).collect()
}

type CatalogResult = Result<(BTreeSet<String>, String), RunnerFailure>;

#[derive(Clone)]
struct CachedCatalog {
    catalog: CatalogResult,
    cached_at: i64,
}

/// One catalog probe in progress: concurrent admissions of the same profile wait for its result
/// instead of each spawning their own `--list-models` child.
#[derive(Default)]
struct InFlightProbe {
    result: Mutex<Option<ModelCatalogProbeResult>>,
    ready: Condvar,
}

impl InFlightProbe {
    fn wait(&self) -> ModelCatalogProbeResult {
        let mut slot = self.result.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if let Some(result) = slot.as_ref() {
                return result.clone();
            }
            slot = self
                .ready
                .wait(slot)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }
}

type InFlightProbes = Mutex<HashMap<String, Arc<InFlightProbe>>>;

/// Publishes the leader's probe result to its waiters and retires the in-flight slot, also when
/// the probe unwinds (waiters then see an aborted-probe failure instead of hanging).
struct ProbeLeader {
    in_flight: Arc<InFlightProbes>,
    key: String,
    flight: Arc<InFlightProbe>,
    outcome: Option<ModelCatalogProbeResult>,
}

impl Drop for ProbeLeader {
    fn drop(&mut self) {
        let outcome = self.outcome.take().unwrap_or_else(|| ModelCatalogProbeResult {
            stderr: "catalog probe aborted".to_string(),
            ..ModelCatalogProbeResult::default()
        });
        *self
            .flight
            .result
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(outcome);
        self.in_flight
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.key);
        self.flight.ready.notify_all();
    }
}

fn build_catalog(model: &str, result: &ModelCatalogProbeResult) -> CatalogResult {
    if result.timed_out {
        Err(admission_failure(model, "catalog probe timed out"))
    } else if result.code != Some(0) {
        let detail = trailing_chars(result.stderr.trim(), 2_000);
        let code = result
            .code
            .map_or_else(|| "null".to_string(), |code| code.to_string());
        let suffix = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        Err(admission_failure(
            model,
            &format!("catalog probe exited {code}{suffix}"),
        ))
    } else {
        Ok((
            parse_model_catalog(&result.stdout),
            trailing_chars(result.stderr.trim(), 1_000),
        ))
    }
}

pub fn create_rpc_model_admission(options: RpcModelAdmissionOptions) -> RpcModelAdmission {
    let build_spawn = options.build_spawn.unwrap_or_else(|| {
        let runtime = RpcSpawnRuntime::default();
        Arc::new(move |spec| build_rpc_model_catalog_spawn(spec, &runtime))
    });
    let probe = options
        .probe
        .unwrap_or_else(|| Arc::new(|descriptor| probe_model_catalog(descriptor, None)));
    let now = options
        .now
        .unwrap_or_else(|| Arc::new(|| chrono::Utc::now().timestamp_millis()));
    let catalogs: Arc<Mutex<HashMap<String, CachedCatalog>>> = Arc::default();
    let in_flight: Arc<InFlightProbes> = Arc::default();
    Arc::new(move |spec| {
        let Some(model) = spec
            .model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
        else {
            return Ok(());
        };
        let descriptor = build_spawn(spec);
        let key = profile_key(&descriptor);
        let fresh = catalogs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .filter(|cached| now() - cached.cached_at < MODEL_CATALOG_CACHE_TTL_MS)
            .cloned();
        let catalog = match fresh {
            Some(cached) => cached.catalog,
            None => {
                let (flight, leads) = {
                    let mut slots = in_flight.lock().unwrap_or_else(PoisonError::into_inner);
                    match slots.get(&key) {
                        Some(flight) => (Arc::clone(flight), false),
                        None => {
                            let flight = Arc::new(InFlightProbe::default());
                            slots.insert(key.clone(), Arc::clone(&flight));
                            (flight, true)
                        }
                    }
                };
                if leads {
                    let mut leader = ProbeLeader {
                        in_flight: Arc::clone(&in_flight),
                        key: key.clone(),
                        flight,
                        outcome: None,
                    };
                    let result = probe(&descriptor);
                    let catalog = build_catalog(model, &result);
                    let cached_at = now();
                    let mut cache = catalogs.lock().unwrap_or_else(PoisonError::into_inner);
                    // Expired entries are already ignored by lookups; evict them so distinct
                    // profiles (per-task cwd) do not accumulate for the runner's lifetime.
                    cache.retain(|_, cached| {
                        cached_at - cached.cached_at < MODEL_CATALOG_CACHE_TTL_MS
                    });
                    cache.insert(
                        key.clone(),
                        CachedCatalog {
                            catalog: catalog.clone(),
                            cached_at,
                        },
                    );
                    drop(cache);
                    // The cache is filled before the slot is retired, so a later arrival hits it.
                    leader.outcome = Some(result);
                    drop(leader);
                    catalog
                } else {
                    build_catalog(model, &flight.wait())
                }
            }
        };
        let verdict = catalog.and_then(|(models, stderr_tail)| {
            if models.contains(model) {
                return Ok(());
            }
            let stderr = if stderr_tail.is_empty() {
                String::new()
            } else {
                format!("; child stderr: {stderr_tail}")
            };
            Err(admission_failure(
                model,
                &format!(
                    "model is not visible in the child profile (probed catalog has {} models{stderr}); forward its provider extension or child-visible settings",
                    models.len()
                ),
            ))
        });
        if verdict.is_err() {
            catalogs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&key);
        }
        verdict
    })
}
