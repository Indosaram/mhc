//! Port of `packages/omo-senpi/src/components/telemetry/omo-native-session.ts`.
//!
//! The one enablement decision (env opt-out matrix + automation gate + omo config gate), the expanded
//! inventory, and the `session_started` event built from it.

pub type RealpathCallback = Option<Arc<dyn Fn(&Path) -> Option<PathBuf> + Send + Sync>>;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};

pub use crate::session_inventory::{
    InventoryDiagnostics, OmoNativeInventoryDiagnostic as InventoryDiagnostic,
};
use crate::account_inventory::{EngineTables, read_connected_accounts};
use crate::index::{SenpiTelemetryOptions, get_senpi_telemetry_state_dir};
use crate::product_identity::{
    EVENT_PROPERTY_ALLOWLISTS, OMO_NATIVE_SCHEMA_VERSION, OmoNativeAttributionInput, create_omo_native_product_config,
    get_omo_native_attribution, get_omo_native_state_dir, hash_session_id,
    with_omo_native_attribution,
};
use crate::session_inventory::read_omo_native_inventory;
use crate::session_role::TelemetrySessionRole;
use crate::session_summary_purge::{ConfigSelection, with_config_selection};

pub const SOURCE: &str = "omo-native-session";
pub const SESSION_REASONS: [&str; 5] = ["startup", "reload", "new", "resume", "fork"];
pub const OPTED_OUT_SESSION_HASH: &str = "opted-out";
const ALLOW_AUTOMATION_VALUES: [&str; 3] = ["1", "true", "yes"];
const POSIX_TEMP_ROOTS: [&str; 4] = ["/tmp", "/private/tmp", "/var/folders", "/private/var/folders"];
const POSIX_USERS_DIRS: [&str; 3] = ["/Users", "/home", "/var/home"];
const POSIX_ROOT_HOMES: [&str; 2] = ["/root", "/var/root"];

/// What the temp-home check reads from the OS; injected only by tests that simulate another platform or
/// filesystem.
#[derive(Clone, Default)]
pub struct TempDirDeps {
    pub platform: Option<String>,
    pub tmpdir: Option<Arc<dyn Fn() -> String + Send + Sync>>,
    pub posix_temp_roots: Option<Vec<String>>,
    pub realpath: RealpathCallback,
}

pub fn session_reason(payload: &Value) -> &'static str {
    match payload.get("reason").and_then(Value::as_str) {
        Some(reason) if SESSION_REASONS.contains(&reason) => {
            SESSION_REASONS
                .into_iter()
                .find(|candidate| *candidate == reason)
                .unwrap_or("startup")
        }
        _ => "startup",
    }
}

pub fn memory_bucket(bytes: u64) -> &'static str {
    let gib = bytes / 1024 / 1024 / 1024;
    match gib {
        0..8 => "lt_8_gb",
        8..16 => "8_15_gb",
        16..32 => "16_31_gb",
        32..64 => "32_63_gb",
        _ => "64_plus_gb",
    }
}

/// Automated runs cannot hold a stable anonymous id (a CI job, a test runner, or a HOME inside the OS temp
/// dir starts from nothing each time), so each one would count as a new person. Telemetry is off for them by
/// default; `OMO_TELEMETRY_ALLOW_AUTOMATION=1` opts a deliberate run back in.
pub fn automated_run_reason(
    env: &telemetry_core::TelemetryEnv,
    deps: &TempDirDeps,
) -> Option<&'static str> {
    if ALLOW_AUTOMATION_VALUES.contains(
        &env.get("OMO_TELEMETRY_ALLOW_AUTOMATION")
            .map(|value| value.trim().to_lowercase())
            .unwrap_or_default()
            .as_str(),
    ) {
        return None;
    }
    let ci = env
        .get("CI")
        .map(|value| value.trim().to_lowercase())
        .unwrap_or_default();
    if !ci.is_empty() && ci != "0" && ci != "false" {
        return Some("ci");
    }
    if env.get("NODE_ENV").map(|value| value.trim()) == Some("test")
        || env.get("VITEST").is_some()
        || env.get("JEST_WORKER_ID").is_some()
    {
        return Some("test_runner");
    }
    let platform = deps.platform.clone().unwrap_or_else(|| {
        if cfg!(target_os = "windows") {
            "win32".into()
        } else {
            "posix".into()
        }
    });
    let home_key = if platform == "win32" {
        "USERPROFILE"
    } else {
        "HOME"
    };
    if let Some(home) = env
        .get(home_key)
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        && is_inside_temp_dir(Path::new(home), env, deps)
    {
        return Some("ephemeral_home");
    }
    None
}

/// Whether `path` (the HOME being classified) is a home that cannot hold a stable install id.
///
/// - A HOME that cannot be resolved counts as ephemeral, so the check fails toward telemetry OFF.
/// - A real account profile, a direct child of the OS users directory or root's home, is never ephemeral,
///   whatever TMPDIR says.
/// - Otherwise it is ephemeral when it lies inside the temp dir (or a fixed POSIX temp root). A temp root that
///   resolves to a filesystem root is ignored.
pub fn is_inside_temp_dir(
    path: &Path,
    env: &telemetry_core::TelemetryEnv,
    deps: &TempDirDeps,
) -> bool {
    let windows = deps.platform.as_deref() == Some("win32");
    let realpath = deps
        .realpath
        .clone()
        .unwrap_or_else(|| Arc::new(|candidate: &Path| std::fs::canonicalize(candidate).ok()));
    let resolve = |candidate: &Path| realpath(candidate);
    let Some(target) = resolve(path) else {
        return true;
    };
    let users_dirs: Vec<PathBuf> = if windows {
        let drive = env
            .get("SystemDrive")
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .unwrap_or("C:");
        vec![PathBuf::from(format!("{drive}\\Users"))]
    } else {
        POSIX_USERS_DIRS.iter().map(PathBuf::from).collect()
    };
    let root_homes: Vec<PathBuf> = if windows {
        Vec::new()
    } else {
        POSIX_ROOT_HOMES.iter().map(PathBuf::from).collect()
    };
    if users_dirs.iter().any(|dir| {
        target
            .parent()
            .and_then(&resolve)
            .zip(resolve(dir))
            .is_some_and(|(parent, dir)| parent == dir)
    }) {
        return false;
    }
    if root_homes
        .iter()
        .any(|home| resolve(home).is_some_and(|home| home == target))
    {
        return false;
    }
    let tmpdir = deps
        .tmpdir
        .as_ref()
        .map(|tmpdir| tmpdir())
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().into_owned());
    let mut candidates = vec![tmpdir];
    if !windows {
        candidates.extend(
            deps.posix_temp_roots
                .clone()
                .unwrap_or_else(|| POSIX_TEMP_ROOTS.iter().map(|root| (*root).to_string()).collect()),
        );
    }
    candidates.into_iter().any(|candidate| {
        let candidate = PathBuf::from(candidate);
        let Some(root) = resolve(&candidate) else {
            return false;
        };
        if root.parent().is_none() {
            return false;
        }
        let Some(candidate) = resolve(&candidate) else {
            return false;
        };
        target == candidate || target.starts_with(&candidate)
    })
}

/// The automation check sits on the consent path, which must never panic: if it cannot decide, telemetry is off.
pub fn not_automated(env: &telemetry_core::TelemetryEnv, deps: &TempDirDeps) -> bool {
    automated_run_reason(env, deps).is_none()
}

fn env_of(options: &SenpiTelemetryOptions) -> telemetry_core::TelemetryEnv {
    options
        .env
        .clone()
        .unwrap_or_else(|| std::env::vars().collect())
}

/// The one enablement decision: the env opt-out matrix plus the omo config gate for the event's cwd.
pub fn is_omo_native_telemetry_enabled(
    options: &SenpiTelemetryOptions,
    cwd: Option<&Path>,
    env: &telemetry_core::TelemetryEnv,
) -> bool {
    not_automated(env, &TempDirDeps::default())
        && telemetry_core::is_telemetry_client_enabled(
            &telemetry_core::TelemetryClientEnabledInput::for_product(
                Some(env),
                &create_omo_native_product_config(),
            ),
        )
        && config_enabled(options, cwd, env)
}

/// The persisted half of consent: the telemetry setting in the config files, for a scope's cwd. Environment
/// opt-outs are left out on purpose: they belong to one process, while this decides for every process that
/// shares the queue. For the same reason the config is read with a neutral env: no profile selection and the
/// account's own home, so a config only this process selects cannot judge other sessions' records.
pub fn is_omo_native_telemetry_config_enabled(
    options: &SenpiTelemetryOptions,
    cwd: Option<&Path>,
    env: &telemetry_core::TelemetryEnv,
    account_home_dir: Option<&Path>,
    selection: Option<&ConfigSelection>,
) -> bool {
    let mut shared = env.clone();
    for key in crate::session_summary_purge::CONFIG_SELECTION_ENV_KEYS {
        shared.remove(key);
    }
    // The account's own home, unless the record names the home its writer read.
    let own_home = selection
        .map(|selection| {
            !selection.contains_key("HOME") && !selection.contains_key("USERPROFILE")
        })
        .unwrap_or(true);
    if own_home && let Some(account_home_dir) = account_home_dir {
        shared.insert(
            "HOME".into(),
            account_home_dir.to_string_lossy().into_owned(),
        );
    }
    let env = match selection {
        None => shared,
        Some(selection) => with_config_selection(&shared, selection),
    };
    config_enabled(options, cwd, &env)
}

fn config_enabled(
    options: &SenpiTelemetryOptions,
    cwd: Option<&Path>,
    env: &telemetry_core::TelemetryEnv,
) -> bool {
    if let Some(is_config_enabled) = &options.is_config_enabled {
        return is_config_enabled(cwd.unwrap_or_else(|| Path::new("")));
    }
    let loaded = maho_omo_config_resolution::load_senpi_omo_config(
        omo_config_core::LoadOmoConfigOptions {
            cwd: cwd.map(|cwd| cwd.to_string_lossy().into_owned()),
            env: Some(env.iter().map(|(key, value)| (key.clone(), value.clone())).collect()),
            ..Default::default()
        },
    );
    if !loaded.diagnostics.is_empty() {
        if let Some(diagnostics) = &options.diagnostics {
            diagnostics(&telemetry_core::TelemetryDiagnosticInput {
                event: telemetry_core::TelemetryDiagnosticEvent::TelemetryCaptureFailed,
                source: SOURCE.into(),
                error: Some(telemetry_core::TelemetryError::new(format!(
                    "{:?}",
                    loaded.diagnostics
                ))),
                error_kind: Some(telemetry_core::TelemetryDiagnosticErrorKind::Error),
                entry_kind: None,
                error_code: None,
                count: None,
            });
        }
        return false;
    }
    omo_config_core::is_omo_telemetry_enabled(&loaded.config)
}

/// A runtime without zone data (or a host that refuses the lookup) simply ships no timezone.
pub fn read_time_zone(provider: Option<&(dyn Fn() -> String + Send + Sync)>) -> Option<String> {
    let zone = match provider {
        Some(provider) => provider(),
        None => std::env::var("TZ").unwrap_or_default(),
    };
    let zone = zone.trim();
    (!zone.is_empty() && zone.len() <= 64).then(|| zone.to_string())
}

pub struct StartNativeSessionInput<'a> {
    pub options: &'a SenpiTelemetryOptions,
    pub session_id: &'a str,
    pub payload: &'a Value,
    pub agent_dir: &'a Path,
    pub session_role: TelemetrySessionRole,
}

pub fn start_native_session(
    input: &StartNativeSessionInput<'_>,
) -> Result<Option<telemetry_core::EventTelemetryClient>, telemetry_core::TelemetryError> {
    use telemetry_core::*;
    let options = input.options;
    let env = env_of(options);
    let product = create_omo_native_product_config();
    if !is_omo_native_telemetry_enabled(options, None, &env) {
        return Ok(None);
    }
    let os = options
        .os_provider
        .as_deref()
        .unwrap_or(get_default_telemetry_os_provider());
    let attribution = get_omo_native_attribution(&OmoNativeAttributionInput {
        env: Some(&env),
        state_dir: None,
        now: options.now,
        os_provider: Some(os),
    });
    let product = with_omo_native_attribution(
        product,
        &OmoNativeAttributionInput {
            env: Some(&env),
            state_dir: None,
            now: options.now,
            os_provider: Some(os),
        },
    );
    let mut allowlists: EventPropertyAllowlist = EVENT_PROPERTY_ALLOWLISTS
        .iter()
        .map(|(name, keys)| {
            (
                (*name).into(),
                keys.iter().map(|key| (*key).into()).collect(),
            )
        })
        .collect();
    allowlists.insert(
        "parallelism_summary".into(),
        crate::parallelism_schema::PARALLELISM_SUMMARY_SCHEMA
            .iter()
            .map(|(key, _)| (*key).into())
            .collect(),
    );
    let client = create_event_telemetry_client(&CreateEventTelemetryClientInput {
        diagnostics: options.diagnostics.clone(),
        distinct_id: &attribution.install_hash,
        env: Some(&env),
        on_capture: None,
        product: &product,
        property_allowlist: &allowlists,
        schema_version: OMO_NATIVE_SCHEMA_VERSION,
        set_timeout_fn: options.set_timeout_fn.clone(),
        source: SOURCE,
        transport_factory: options.transport_factory.clone(),
    });
    if !client.enabled() {
        return Ok(None);
    }
    let state = options
        .state_dir
        .clone()
        .unwrap_or_else(|| get_omo_native_state_dir(&env));
    let hash =
        hash_session_id(input.session_id, &state).map_err(|e| TelemetryError::new(e.to_string()))?;
    let activity = get_daily_active_capture_state(&DailyActiveCaptureStateInput {
        state_dir: &state,
        now: options.now,
        diagnostics: options
            .diagnostics
            .as_deref()
            .map(|diagnostics| diagnostics as &dyn Fn(&TelemetryDiagnosticInput)),
    });
    if activity.capture_daily {
        let properties = serde_json::Map::from_iter([
            ("$session_id".into(), json!(hash)),
            ("day_utc".into(), json!(activity.day_utc)),
            ("reason".into(), json!("session_start")),
        ]);
        client.capture_event("daily_active", &properties);
    }
    let inventory = read_omo_native_inventory(input.agent_dir, options.inventory_diagnostics.as_ref());
    let connected_accounts = read_connected_accounts(
        input.agent_dir,
        &env,
        EngineTables::load().as_ref(),
    );
    let reason = session_reason(input.payload);
    let timezone = read_time_zone(options.time_zone.as_deref());
    let mut properties = serde_json::Map::from_iter([
        ("$session_id".into(), json!(hash)),
        ("$os".into(), json!(os.platform())),
        ("$os_version".into(), json!(os.release())),
        ("arch".into(), json!(os.arch())),
        ("cpu_count".into(), json!(os.cpus()?.len())),
        ("memory_bucket".into(), json!(memory_bucket(os.totalmem()))),
        ("provider_count".into(), json!(inventory.provider_count)),
        (
            "signed_in_provider_count".into(),
            json!(inventory.signed_in_provider_count),
        ),
        (
            "configured_provider_count".into(),
            json!(inventory.configured_provider_count),
        ),
        (
            "connected_account_count".into(),
            json!(connected_accounts),
        ),
        ("model_count".into(), json!(inventory.model_count)),
        ("providers".into(), json!(inventory.providers)),
        ("reason".into(), json!(reason)),
        ("session_role".into(), json!(input.session_role.as_str())),
    ]);
    if let Some(timezone) = timezone {
        properties.insert("timezone".into(), json!(timezone));
    }
    if let Some(default_provider) = inventory.default_provider {
        properties.insert("default_provider".into(), json!(default_provider));
    }
    if let Some(default_model) = inventory.default_model {
        properties.insert("default_model".into(), json!(default_model));
    }
    client.capture_event("session_started", &properties);
    let mut legacy = options.clone();
    legacy.state_dir = Some(get_senpi_telemetry_state_dir(&env));
    tokio::spawn(async move {
        if let Err(error) = crate::index::record_senpi_daily_active(&legacy).await {
            maho_ext_api::diagnostic(&format!("omo-senpi legacy telemetry failed: {error}"));
        }
    });
    Ok(Some(client))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> telemetry_core::TelemetryEnv {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn deps(tmpdir: &Path, roots: &[&Path]) -> TempDirDeps {
        let tmp = tmpdir.to_path_buf();
        let roots: Vec<String> = roots
            .iter()
            .map(|root| root.to_string_lossy().into_owned())
            .collect();
        TempDirDeps {
            platform: Some("linux".into()),
            tmpdir: Some(Arc::new(move || tmp.to_string_lossy().into_owned())),
            posix_temp_roots: Some(roots),
            realpath: Some(Arc::new(|candidate: &Path| {
                std::fs::canonicalize(candidate).ok()
            })),
        }
    }

    #[test]
    fn memory_thresholds() {
        for (gib, expected) in [
            (0, "lt_8_gb"),
            (7, "lt_8_gb"),
            (8, "8_15_gb"),
            (16, "16_31_gb"),
            (32, "32_63_gb"),
            (64, "64_plus_gb"),
        ] {
            assert_eq!(memory_bucket(gib * 1024 * 1024 * 1024), expected);
        }
    }

    #[test]
    fn reason_validation() {
        for reason in SESSION_REASONS {
            assert_eq!(session_reason(&json!({"reason": reason})), reason);
        }
        assert_eq!(session_reason(&json!({"reason": "secret"})), "startup");
        assert_eq!(session_reason(&json!({})), "startup");
    }

    #[test]
    fn automation_gating_fails_closed() {
        assert_eq!(
            automated_run_reason(&env(&[("CI", "1")]), &TempDirDeps::default()),
            Some("ci")
        );
        assert_eq!(
            automated_run_reason(&env(&[("CI", "false")]), &TempDirDeps::default()),
            None
        );
        assert_eq!(
            automated_run_reason(&env(&[("CI", "0")]), &TempDirDeps::default()),
            None
        );
        assert_eq!(
            automated_run_reason(&env(&[("VITEST", "1")]), &TempDirDeps::default()),
            Some("test_runner")
        );
        assert_eq!(
            automated_run_reason(
                &env(&[("NODE_ENV", "test")]),
                &TempDirDeps::default()
            ),
            Some("test_runner")
        );
        assert_eq!(
            automated_run_reason(&env(&[("JEST_WORKER_ID", "2")]), &TempDirDeps::default()),
            Some("test_runner")
        );
        // An explicit override opts a deliberate run back in, whatever else is set.
        for value in ["1", "true", "yes", " TRUE "] {
            assert_eq!(
                automated_run_reason(
                    &env(&[
                        ("CI", "1"),
                        ("VITEST", "1"),
                        ("OMO_TELEMETRY_ALLOW_AUTOMATION", value),
                    ]),
                    &TempDirDeps::default()
                ),
                None,
                "{value:?}"
            );
        }
        for value in ["0", "false", "no", ""] {
            assert_eq!(
                automated_run_reason(
                    &env(&[("CI", "1"), ("OMO_TELEMETRY_ALLOW_AUTOMATION", value)]),
                    &TempDirDeps::default()
                ),
                Some("ci"),
                "{value:?}"
            );
        }
    }

    #[test]
    fn ephemeral_homes_are_detected_and_real_account_homes_are_not() {
        let tmp = tempfile::tempdir().unwrap();
        let deps = deps(tmp.path(), &[]);
        let inside = tmp.path().join("runner-home");
        std::fs::create_dir_all(&inside).unwrap();
        assert!(is_inside_temp_dir(&inside, &env(&[]), &deps));
        assert_eq!(
            automated_run_reason(
                &env(&[("HOME", inside.to_str().unwrap())]),
                &deps
            ),
            Some("ephemeral_home")
        );
        // A real account profile is never ephemeral, whatever TMPDIR says.
        let account_deps = TempDirDeps {
            platform: Some("linux".into()),
            tmpdir: Some(Arc::new(|| "/tmp".into())),
            posix_temp_roots: Some(vec!["/home".into()]),
            realpath: Some(Arc::new(|candidate: &Path| {
                if candidate == Path::new("/fixture-home") {
                    Some(PathBuf::from("/home/alice"))
                } else {
                    Some(candidate.to_path_buf())
                }
            })),
        };
        assert!(!is_inside_temp_dir(
            Path::new("/fixture-home"),
            &env(&[]),
            &account_deps
        ));
        // Root's home is likewise never ephemeral.
        assert!(!is_inside_temp_dir(Path::new("/root"), &env(&[]), &account_deps));
    }

    #[test]
    fn an_unresolvable_home_counts_as_ephemeral() {
        let deps = TempDirDeps {
            platform: Some("linux".into()),
            tmpdir: Some(Arc::new(|| "/tmp".into())),
            posix_temp_roots: Some(Vec::new()),
            realpath: Some(Arc::new(|_: &Path| None)),
        };
        assert!(is_inside_temp_dir(
            Path::new("/definitely/missing"),
            &env(&[]),
            &deps
        ));
    }

    #[test]
    fn a_filesystem_root_temp_dir_is_ignored() {
        let root = Path::new("/");
        let deps = TempDirDeps {
            platform: Some("linux".into()),
            tmpdir: Some(Arc::new(|| "/".into())),
            posix_temp_roots: Some(Vec::new()),
            realpath: Some(Arc::new(|candidate: &Path| {
                std::fs::canonicalize(candidate).ok()
            })),
        };
        let home = tempfile::tempdir().unwrap();
        assert!(!is_inside_temp_dir(home.path(), &env(&[]), &deps));
        assert_eq!(root, Path::new("/"));
    }

    #[test]
    fn windows_homes_use_userprofile() {
        let tmp = tempfile::tempdir().unwrap();
        let deps = TempDirDeps {
            platform: Some("win32".into()),
            tmpdir: Some(Arc::new({ let path = tmp.path().to_string_lossy().into_owned(); move || path.clone() })),
            posix_temp_roots: None,
            realpath: Some(Arc::new(|candidate: &Path| {
                std::fs::canonicalize(candidate).ok()
            })),
        };
        let inside = tmp.path().join("Temp-home");
        std::fs::create_dir_all(&inside).unwrap();
        assert_eq!(
            automated_run_reason(
                &env(&[("USERPROFILE", inside.to_str().unwrap())]),
                &deps
            ),
            Some("ephemeral_home")
        );
        // HOME is not read on win32.
        assert_eq!(
            automated_run_reason(&env(&[("HOME", inside.to_str().unwrap())]), &deps),
            None
        );
    }

    #[test]
    fn timezone_reads_the_provider_then_the_environment() {
        assert_eq!(read_time_zone(None), std::env::var("TZ").ok().filter(|zone| {
            !zone.trim().is_empty() && zone.trim().len() <= 64
        }).map(|zone| zone.trim().to_string()));
        assert_eq!(
            read_time_zone(Some(&|| "Europe/Berlin".to_string())).as_deref(),
            Some("Europe/Berlin")
        );
        assert_eq!(read_time_zone(Some(&|| String::new())), None);
        assert_eq!(
            read_time_zone(Some(&|| "x".repeat(65))),
            None
        );
    }
}
