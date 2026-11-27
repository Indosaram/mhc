//! `live-config-provider.ts`: cached, error-resilient live `OmoConfig` reloader for the task planner.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

use maho_omo_config_resolution::{SenpiConfigDiagnostic, SenpiOmoConfigResult};
use omo_config_core::MERGED_OMO_CONFIG_PATH;
use serde_json::{Value, json};

pub type LiveConfigWarn = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct LiveConfigProviderOptions {
    pub cwd: PathBuf,
    pub load_config: Arc<dyn Fn(&Path) -> SenpiOmoConfigResult + Send + Sync>,
    pub initial_result: SenpiOmoConfigResult,
    pub logger: Option<LiveConfigWarn>,
    pub cache_ttl_ms: Option<i64>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

fn diagnostic_fields(diagnostic: &SenpiConfigDiagnostic) -> (&str, &str, &str) {
    match diagnostic {
        SenpiConfigDiagnostic::Config(config) => {
            (config.kind, config.path.as_str(), config.message.as_str())
        }
        SenpiConfigDiagnostic::Model(model) => {
            (model.kind, model.path.as_str(), model.message.as_str())
        }
    }
}

/// Error severity: the loader could not apply what the file says (a layer that failed to read or
/// parse, or a file or merged config rejected whole by validation), or a model catalog reference
/// cycles.
fn is_error_diagnostic(diagnostic: &SenpiConfigDiagnostic, result: &SenpiOmoConfigResult) -> bool {
    let (kind, path, _) = diagnostic_fields(diagnostic);
    match kind {
        "parse" | "read" | "model_catalog_cycle" => true,
        "validation" => {
            path == MERGED_OMO_CONFIG_PATH
                || result
                    .loaded
                    .sources
                    .iter()
                    .any(|source| source.path == path)
        }
        _ => false,
    }
}

fn fingerprint(diagnostic: &SenpiConfigDiagnostic) -> String {
    let (kind, path, message) = diagnostic_fields(diagnostic);
    format!("{kind}\u{0}{path}\u{0}{message}")
}

fn error_fingerprints(result: &SenpiOmoConfigResult) -> BTreeSet<String> {
    result
        .diagnostics
        .iter()
        .filter(|diagnostic| is_error_diagnostic(diagnostic, result))
        .map(fingerprint)
        .collect()
}

fn default_warn(message: &str, meta: Value) {
    maho_ext_api::diagnostic(&format!("{message} ({meta})"));
}

struct ProviderState {
    last_good_config: Value,
    adopted_errors: BTreeSet<String>,
    cached_config: Value,
    last_cached_time: i64,
    last_warning: Option<String>,
}

/// Creates a cached, resilient live `OmoConfig` provider for the task planner:
/// - Caches config reads within `cache_ttl_ms` (default 250ms) to avoid redundant reloads during a plan.
/// - Rejects a reload only for an error-severity diagnostic the adopted config did not already carry.
/// - On a rejected or panicked reload, retains the last adopted config and warns once per distinct cause.
pub fn create_live_config_provider(
    options: LiveConfigProviderOptions,
) -> Arc<dyn Fn() -> Value + Send + Sync> {
    let cwd = options.cwd;
    let load_config = options.load_config;
    let cache_ttl_ms = options.cache_ttl_ms.unwrap_or(250);
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = options
        .now
        .unwrap_or_else(|| Arc::new(|| chrono::Utc::now().timestamp_millis()));
    let logger: LiveConfigWarn = options.logger.unwrap_or_else(|| Arc::new(default_warn));
    let initial_time = now();
    let initial_errors = error_fingerprints(&options.initial_result);
    let initial_config = options.initial_result.config;
    let state = Mutex::new(ProviderState {
        last_good_config: initial_config.clone(),
        adopted_errors: initial_errors,
        cached_config: initial_config,
        last_cached_time: initial_time,
        last_warning: None,
    });

    Arc::new(move || {
        let mut guard = state.lock().unwrap_or_else(PoisonError::into_inner);
        let current = now();
        if current.saturating_sub(guard.last_cached_time) < cache_ttl_ms {
            return guard.cached_config.clone();
        }
        guard.last_cached_time = current;

        let loaded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (load_config)(&cwd)));
        match loaded {
            Ok(reloaded) => {
                let mut introduced: Vec<String> = reloaded
                    .diagnostics
                    .iter()
                    .filter(|diagnostic| {
                        is_error_diagnostic(diagnostic, &reloaded)
                            && !guard.adopted_errors.contains(&fingerprint(diagnostic))
                    })
                    .map(fingerprint)
                    .collect();
                if introduced.is_empty() {
                    guard.adopted_errors = error_fingerprints(&reloaded);
                    guard.last_good_config = reloaded.config.clone();
                    guard.cached_config = reloaded.config;
                    guard.last_warning = None;
                } else {
                    introduced.sort();
                    let key = format!("errors\u{1}{}", introduced.join("\u{1}"));
                    if guard.last_warning.as_deref() != Some(key.as_str()) {
                        guard.last_warning = Some(key);
                        let details: Vec<Value> = reloaded
                            .diagnostics
                            .iter()
                            .filter(|diagnostic| is_error_diagnostic(diagnostic, &reloaded))
                            .map(|diagnostic| {
                                let (kind, path, message) = diagnostic_fields(diagnostic);
                                json!({ "kind": kind, "path": path, "message": message })
                            })
                            .collect();
                        logger(
                            "omo-senpi task: live config reload encountered errors; retaining last good configuration",
                            json!({ "diagnostics": details }),
                        );
                    }
                    guard.cached_config = guard.last_good_config.clone();
                }
            }
            Err(payload) => {
                let error_text = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|| "panic during reload".to_string());
                let key = format!("thrown\u{1}{error_text}");
                if guard.last_warning.as_deref() != Some(key.as_str()) {
                    guard.last_warning = Some(key);
                    logger(
                        "omo-senpi task: live config reload failed; retaining last good configuration",
                        json!({ "error": error_text }),
                    );
                }
                guard.cached_config = guard.last_good_config.clone();
            }
        }
        guard.cached_config.clone()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use omo_config_core::{LoadOmoConfigResult, OmoConfigDiagnostic, OmoConfigSource};
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    fn make_result(
        config: Value,
        diagnostics: Vec<SenpiConfigDiagnostic>,
        paths: Vec<&str>,
    ) -> SenpiOmoConfigResult {
        SenpiOmoConfigResult {
            loaded: LoadOmoConfigResult {
                config: Default::default(),
                diagnostics: Vec::new(),
                layers: Vec::new(),
                profile: None,
                sources: paths
                    .into_iter()
                    .map(|p| OmoConfigSource {
                        exists: true,
                        loaded: true,
                        path: p.into(),
                        scope: "project",
                    })
                    .collect(),
            },
            config,
            diagnostics,
        }
    }

    #[test]
    fn caches_within_ttl_then_reloads_and_keeps_last_good_on_error_with_single_warning() {
        let clock = Arc::new(AtomicI64::new(1000));
        let loads = Arc::new(AtomicUsize::new(0));
        let warnings = Arc::new(Mutex::new(Vec::<String>::new()));

        let loads_ref = loads.clone();
        let warn_ref = warnings.clone();
        let clock_ref = clock.clone();

        let provider = create_live_config_provider(LiveConfigProviderOptions {
            cwd: PathBuf::from("/tmp"),
            load_config: Arc::new(move |_| {
                let call = loads_ref.fetch_add(1, Ordering::SeqCst);
                if call == 0 {
                    make_result(json!({"tag": "updated"}), Vec::new(), Vec::new())
                } else {
                    make_result(
                        json!({"tag": "broken"}),
                        vec![SenpiConfigDiagnostic::Config(OmoConfigDiagnostic {
                            kind: "parse",
                            path: "/tmp/omo.json".into(),
                            message: "syntax error".into(),
                            issue_paths: Vec::new(),
                        })],
                        vec!["/tmp/omo.json"],
                    )
                }
            }),
            initial_result: make_result(json!({"tag": "initial"}), Vec::new(), Vec::new()),
            logger: Some(Arc::new(move |msg, _| {
                warn_ref
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(msg.to_string());
            })),
            cache_ttl_ms: Some(250),
            now: Some(Arc::new(move || clock_ref.load(Ordering::SeqCst))),
        });

        assert_eq!(provider(), json!({"tag": "initial"}));
        assert_eq!(loads.load(Ordering::SeqCst), 0);

        clock.store(1250, Ordering::SeqCst);
        assert_eq!(provider(), json!({"tag": "updated"}));
        assert_eq!(loads.load(Ordering::SeqCst), 1);

        clock.store(1500, Ordering::SeqCst);
        assert_eq!(provider(), json!({"tag": "updated"}));
        assert_eq!(loads.load(Ordering::SeqCst), 2);
        assert_eq!(
            warnings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            1
        );

        // Same error on next TTL expiry keeps warning deduplicated.
        clock.store(1750, Ordering::SeqCst);
        assert_eq!(provider(), json!({"tag": "updated"}));
        assert_eq!(
            warnings
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .len(),
            1
        );
    }
}
