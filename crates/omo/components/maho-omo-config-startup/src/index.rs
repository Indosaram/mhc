use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use config_migration::{
    ConfigMigrationDiscoveryOptions, CreateLegacyConfigMigrationPlansOptions, PathOperations,
    create_legacy_config_migration_plans,
};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, NotificationType};
use model_core::{DEVIN_SWE2_SERVED_LANES, is_unserved_devin_swe2_selector};
use omo_config_core::*;
use serde_json::Value;

#[derive(Default)]
pub struct SenpiStartupMigrationResult {
    pub error: Option<String>,
    pub journal_resumed: bool,
    pub migrated_from: Vec<String>,
    pub results: Vec<MigrationRunResult>,
}

#[derive(Default)]
pub struct SenpiStartupMigrationOptions<'a> {
    pub backup_timestamp: Option<&'a str>,
    pub clock: Option<&'a dyn MigrationClock>,
    pub discovery_file_system: Option<&'a dyn config_migration::ConfigMigrationDiscoveryFileSystem>,
    pub file_system: Option<&'a dyn MigrationFileSystem>,
    pub pid: Option<u32>,
    pub is_process_alive: Option<Box<dyn Fn(u32) -> bool + 'a>>,
    pub on_boundary: Option<Box<MigrationBoundaryHook<'a>>>,
    pub platform: Option<config_migration::Platform>,
}

pub fn run_senpi_startup_migration(
    cwd: &str,
    environment: &BTreeMap<String, String>,
    home_dir: &str,
) -> SenpiStartupMigrationResult {
    run_senpi_startup_migration_with_options(
        cwd,
        environment,
        home_dir,
        SenpiStartupMigrationOptions::default(),
    )
}

pub fn run_senpi_startup_migration_with_options<'a>(
    cwd: &'a str,
    environment: &'a BTreeMap<String, String>,
    home_dir: &'a str,
    options: SenpiStartupMigrationOptions<'a>,
) -> SenpiStartupMigrationResult {
    if home_dir.is_empty() {
        return SenpiStartupMigrationResult {
            error: Some("Cannot migrate configuration because no home directory is available".into()),
            ..Default::default()
        };
    }
    let discovery_error = std::cell::RefCell::new(None);
    let batch = run_migrations(RunMigrationsOptions {
        after_migrations: None,
        clock: options.clock,
        discover: Box::new(|| {
            let plans = match create_legacy_config_migration_plans(
                &CreateLegacyConfigMigrationPlansOptions {
                    backup_timestamp: options.backup_timestamp,
                    discovery: ConfigMigrationDiscoveryOptions {
                        cwd,
                        environment,
                        file_system: options.discovery_file_system,
                        home_dir,
                        path_operations: if options.platform
                            == Some(config_migration::Platform::Win32)
                        {
                            PathOperations::Win32
                        } else {
                            PathOperations::Posix
                        },
                        platform: options.platform,
                        tauri_config_dirs: None,
                    },
                },
            ) {
                Ok(plans) => plans,
                Err(error) => {
                    *discovery_error.borrow_mut() = Some(error.to_string());
                    return Vec::new();
                }
            };
            plans
                .iter()
                .map(|plan| {
                    let transform = Rc::clone(&plan.transform);
                    MigrationPlan {
                        id: plan.id.clone(),
                        mode: plan.mode,
                        should_run: plan.should_run.clone(),
                        sources: plan.sources.clone(),
                        target_path: plan.target_path.clone(),
                        transform: Box::new(move |loaded| {
                            let result = transform(loaded)?;
                            Ok(MigrationTransformResult {
                                diagnostics: result.diagnostics,
                                document: Value::Object(result.document),
                            })
                        }),
                    }
                })
                .collect()
        }),
        dry_run: false,
        env: Some(environment.clone()),
        file_system: options.file_system,
        is_process_alive: options.is_process_alive,
        lease_duration_ms: None,
        on_boundary: options.on_boundary,
        pid: options.pid,
        write_target: None,
    });
    if let Some(error) = discovery_error.into_inner() {
        return SenpiStartupMigrationResult {
            error: Some(error),
            ..Default::default()
        };
    }
    match batch {
        Err(error) => SenpiStartupMigrationResult {
            error: Some(error.to_string()),
            ..Default::default()
        },
        Ok(batch) => {
            let migrated_from: BTreeSet<_> = batch
                .results
                .iter()
                .filter(|result| result.status == MigrationStatus::Migrated)
                .flat_map(|result| {
                    result
                        .preview
                        .iter()
                        .flat_map(|preview| preview.backup_moves.iter().map(|m| m.from.clone()))
                })
                .collect();
            SenpiStartupMigrationResult {
                error: (batch.status == MigrationBatchStatus::Locked)
                    .then(|| "Configuration migration is already running".into()),
                journal_resumed: batch.journal_resumed,
                migrated_from: migrated_from.into_iter().collect(),
                results: batch.results,
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupNoticeKind {
    Info,
    Warning,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartupNotice {
    pub message: String,
    pub kind: StartupNoticeKind,
}

/// Both halves come from model-core's Devin SWE-2 detector (`DevinSwe2Policy::native`); with either
/// absent the clause is inert.
#[derive(Clone, Copy, Default)]
pub struct DevinSwe2Policy<'a> {
    pub served_lanes: Option<&'a [&'a str]>,
    pub is_unserved: Option<&'a dyn Fn(&str) -> bool>,
}

impl DevinSwe2Policy<'_> {
    pub const fn unavailable() -> Self {
        Self {
            served_lanes: None,
            is_unserved: None,
        }
    }

    fn available(&self) -> bool {
        self.served_lanes.is_some() && self.is_unserved.is_some()
    }
}

impl DevinSwe2Policy<'static> {
    /// The real producer: model-core's SWE-2 served lanes and unserved-selector predicate.
    #[must_use]
    pub fn native() -> Self {
        Self {
            served_lanes: Some(&DEVIN_SWE2_SERVED_LANES),
            is_unserved: Some(&is_unserved_devin_swe2_selector),
        }
    }
}

fn served_devin_lanes(lanes: &[&str]) -> String {
    let rendered: Vec<String> = lanes.iter().map(|lane| format!("devin/{lane}")).collect();
    match rendered.split_last() {
        None => String::new(),
        Some((last, rest)) => format!("{} or {}", rest.join(", "), last),
    }
}

fn list_selectors(value: Option<&Value>, path: &str) -> Vec<(String, String)> {
    match value {
        Some(Value::String(selector)) => vec![(selector.clone(), path.to_string())],
        Some(Value::Array(items)) => items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                let selector = match item {
                    Value::String(selector) => Some(selector.clone()),
                    Value::Object(entry) => {
                        entry.get("model").and_then(Value::as_str).map(str::to_string)
                    }
                    _ => None,
                };
                selector.map(|selector| (selector, format!("{path}[{index}]")))
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn unserved_devin_selectors(
    config: &Value,
    policy: &DevinSwe2Policy<'_>,
) -> Vec<(String, String)> {
    if !policy.available() {
        return Vec::new();
    }
    let Some(is_unserved) = policy.is_unserved else {
        return Vec::new();
    };
    let mut selectors = Vec::new();
    for section in ["categories", "agents"] {
        let Some(entries) = config.get(section).and_then(Value::as_object) else {
            continue;
        };
        for (name, entry) in entries {
            let Some(entry) = entry.as_object() else {
                continue;
            };
            let base = format!("{section}.{name}");
            if let Some(selector) = entry.get("model").and_then(Value::as_str) {
                selectors.push((selector.to_string(), format!("{base}.model")));
            }
            selectors.extend(list_selectors(entry.get("models"), &format!("{base}.models")));
            if entry.contains_key("fallback_models") {
                selectors.extend(list_selectors(
                    entry.get("fallback_models"),
                    &format!("{base}.fallback_models"),
                ));
            }
        }
    }
    selectors
        .into_iter()
        .filter(|(selector, _)| is_unserved(selector))
        .collect()
}

pub fn notification_messages(
    migration: &SenpiStartupMigrationResult,
    config: &maho_omo_config_resolution::SenpiOmoConfigResult,
    devin: &DevinSwe2Policy<'_>,
) -> Vec<StartupNotice> {
    let mut messages = Vec::new();
    if let Some(error) = &migration.error {
        messages.push(StartupNotice {
            message: format!("OmO Native: configuration migration: {error}"),
            kind: StartupNoticeKind::Warning,
        });
    } else if !migration.migrated_from.is_empty() {
        messages.push(StartupNotice {
            message: format!(
                "OmO Native: migrated legacy configuration from {}",
                migration.migrated_from.join(", ")
            ),
            kind: StartupNoticeKind::Info,
        });
    } else if migration.journal_resumed {
        messages.push(StartupNotice {
            message: "OmO Native: recovered an interrupted configuration migration".into(),
            kind: StartupNoticeKind::Info,
        });
    }
    let migration_diagnostics: Vec<&str> = migration
        .results
        .iter()
        .flat_map(|result| result.diagnostics.iter().map(String::as_str))
        .collect();
    if !migration_diagnostics.is_empty() {
        messages.push(StartupNotice {
            message: format!(
                "OmO Native: configuration migration: {}",
                migration_diagnostics.join("; ")
            ),
            kind: StartupNoticeKind::Warning,
        });
    }
    if !config.diagnostics.is_empty() {
        let rendered: Vec<&str> = config
            .diagnostics
            .iter()
            .map(|diagnostic| match diagnostic {
                maho_omo_config_resolution::SenpiConfigDiagnostic::Config(diagnostic) => {
                    diagnostic.message.as_str()
                }
                maho_omo_config_resolution::SenpiConfigDiagnostic::Model(diagnostic) => {
                    diagnostic.message.as_str()
                }
            })
            .collect();
        messages.push(StartupNotice {
            message: format!(
                "OmO Native: configuration diagnostics: {}",
                rendered.join("; ")
            ),
            kind: StartupNoticeKind::Warning,
        });
    }
    let unserved = unserved_devin_selectors(&config.config, devin);
    if !unserved.is_empty() {
        let rendered: Vec<String> = unserved
            .iter()
            .map(|(selector, path)| format!("{selector} ({path})"))
            .collect();
        let lanes = devin
            .served_lanes
            .map_or_else(String::new, served_devin_lanes);
        messages.push(StartupNotice {
            message: format!(
                "OmO Native: Devin does not serve {}; SWE-2 runs as {lanes}",
                rendered.join(", ")
            ),
            kind: StartupNoticeKind::Warning,
        });
    }
    messages
}

pub type StartupMigrationRunner = Arc<dyn Fn(&str) -> SenpiStartupMigrationResult + Send + Sync>;
pub type StartupConfigLoader =
    Arc<dyn Fn(&str) -> maho_omo_config_resolution::SenpiOmoConfigResult + Send + Sync>;

#[derive(Default)]
pub struct ConfigStartupComponent {
    pub run_migration: Option<StartupMigrationRunner>,
    pub load_config: Option<StartupConfigLoader>,
}

impl Extension for ConfigStartupComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let cwd = api.cwd.to_string_lossy().into_owned();
        let env: BTreeMap<_, _> = std::env::vars().collect();
        let home = env
            .get("HOME")
            .or_else(|| env.get("USERPROFILE"))
            .map(String::as_str)
            .unwrap_or_default();
        let migration = self
            .run_migration
            .as_ref()
            .map_or_else(|| run_senpi_startup_migration(&cwd, &env, home), |run| run(&cwd));
        let config = self.load_config.as_ref().map_or_else(
            || {
                maho_omo_config_resolution::load_senpi_omo_config(LoadOmoConfigOptions {
                    cwd: Some(cwd.clone()),
                    env: Some(env),
                    ..Default::default()
                })
            },
            |load| load(&cwd),
        );
let notices = notification_messages(&migration, &config, &DevinSwe2Policy::native());
        let reported = Arc::new(AtomicBool::new(false));
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_, ctx| {
                let reported = Arc::clone(&reported);
                let notices = notices.clone();
                Box::pin(async move {
                    if !notices.is_empty() && !reported.swap(true, Ordering::SeqCst) {
                        for notice in notices {
                            let kind = if notice.kind == StartupNoticeKind::Warning {
                                NotificationType::Warning
                            } else {
                                NotificationType::Info
                            };
                            if ctx.has_ui {
                                ctx.ui.notify(&notice.message, kind);
                            } else {
                                eprintln!("{}", notice.message);
                            }
                        }
                    }
                    Ok(EventResult::None)
                })
            }),
        );
    }
}
