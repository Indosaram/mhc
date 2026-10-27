//! The plan set run at startup (spec §12.2) and the startup wrapper (§12.3).

use super::transforms::{
    category_split, codex_import, harness_rename, needs_provider_rename, provider_rename,
    reasoning_unification,
};
use super::{
    BatchOptions, BatchStatus, MigrationEnv, Plan, PlanMode, PlanResult, PlanSource, PlanStatus,
    TransformOutput, run_migrations,
};
use crate::env::{Env, forward_slashes};
use crate::fs::RealFs;
use crate::legacy_names::{has_legacy_category, has_legacy_harness};
use crate::paths::{MAX_WALK_DEPTH, project_file_in, user_candidate};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Migration ids stored in users' `_migrations` arrays (interface facts).
pub const REASONING_UNIFICATION: &str = "2026-08-reasoning-unification";
pub const CATEGORY_DEEP_SPLIT: &str = "2026-09-category-deep-split";
pub const HARNESS_NATIVE_RENAME: &str = "2026-09-harness-native-rename";
pub const SUBSCRIPTION_PROVIDER_RENAME: &str = "2026-09-subscription-provider-rename";
pub const CODEX_CONFIG_JSONC: &str = "2026-07-codex-config-jsonc";

/// Discovery inputs.
#[derive(Clone, Debug)]
pub struct Discovery {
    pub cwd: PathBuf,
    pub env: Env,
    pub home: PathBuf,
}

fn same_path(a: &Path, b: &Path) -> bool {
    let a = maho_support::canonicalize_lenient(a);
    let b = maho_support::canonicalize_lenient(b);
    if cfg!(windows) {
        forward_slashes(&a.to_string_lossy()).to_lowercase()
            == forward_slashes(&b.to_string_lossy()).to_lowercase()
    } else {
        a == b
    }
}

/// In-place targets: the existing user file, the project files from `cwd` upwards (including
/// home when the walk starts inside it), then the import targets; de-duplicated.
pub fn discover_targets(discovery: &Discovery, import_targets: &[PathBuf]) -> Vec<PathBuf> {
    let fs = RealFs;
    let mut out: Vec<PathBuf> = Vec::new();
    let push = |path: PathBuf, out: &mut Vec<PathBuf>| {
        if !out.iter().any(|existing| same_path(existing, &path)) {
            out.push(path);
        }
    };
    let user = user_candidate(&discovery.home, &fs);
    if user.exists() {
        push(user, &mut out);
    }
    let cwd = maho_support::normalize_lexically(&discovery.cwd);
    let inside_home = maho_support::contains_path(&discovery.home, &cwd);
    let mut current = Some(cwd.as_path());
    let mut visited = 0;
    while let Some(dir) = current {
        if visited >= MAX_WALK_DEPTH {
            break;
        }
        visited += 1;
        if let Some(file) = project_file_in(dir, &fs) {
            push(file, &mut out);
        }
        if inside_home && same_path(dir, &discovery.home) {
            break;
        }
        current = dir.parent();
    }
    for target in import_targets {
        push(target.clone(), &mut out);
    }
    out
}

fn replace_plan(
    id: &str,
    target: &Path,
    predicate: Option<super::Predicate>,
    transform: super::Transform,
) -> Plan {
    Plan {
        id: id.into(),
        mode: PlanMode::ReplaceTarget,
        predicate,
        target: target.to_path_buf(),
        sources: Vec::new(),
        transform,
    }
}

/// The import plan for `<home>/.omo/config.jsonc`.
pub fn codex_import_plan(home: &Path, stamp: &str) -> Plan {
    let omo = home.join(".omo");
    let target = user_candidate(home, &RealFs);
    let backup_dir = omo.join(format!("migration-backup-{stamp}-opencode-config"));
    let source = omo.join("config.jsonc");
    let sidecar = omo.join("config.jsonc.migrations.json");
    let backup_for = |path: &Path| {
        let relative = path
            .strip_prefix(home)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| path.to_path_buf());
        backup_dir.join(relative)
    };
    let sources = vec![
        PlanSource {
            backup: Some(backup_for(&source)),
            path: source.clone(),
        },
        PlanSource {
            backup: Some(backup_for(&sidecar)),
            path: sidecar.clone(),
        },
    ];
    Plan {
        id: CODEX_CONFIG_JSONC.into(),
        mode: PlanMode::Merge,
        predicate: None,
        target,
        sources,
        transform: Arc::new(move |input| {
            let find = |wanted: &Path| {
                input
                    .sources
                    .iter()
                    .find(|(path, _)| path == wanted)
                    .map(|(_, v)| v)
            };
            let (document, diagnostics) = codex_import(find(&source), find(&sidecar));
            Ok(TransformOutput {
                document,
                diagnostics,
            })
        }),
    }
}

/// Every startup plan in execution order: import plans first, then each in-place plan for every
/// target before the next in-place plan.
pub fn startup_migration_plans(discovery: &Discovery, stamp: &str) -> Vec<Plan> {
    let import = codex_import_plan(&discovery.home, stamp);
    let targets = discover_targets(discovery, std::slice::from_ref(&import.target));
    let mut plans = vec![import];
    for target in &targets {
        plans.push(replace_plan(
            REASONING_UNIFICATION,
            target,
            None,
            Arc::new(|input| {
                let (document, diagnostics) = reasoning_unification(input.target)?;
                Ok(TransformOutput {
                    document,
                    diagnostics,
                })
            }),
        ));
    }
    for target in &targets {
        plans.push(replace_plan(
            CATEGORY_DEEP_SPLIT,
            target,
            Some(Arc::new(has_legacy_category)),
            Arc::new(|input| {
                let (document, diagnostics) = category_split(input.target);
                Ok(TransformOutput {
                    document,
                    diagnostics,
                })
            }),
        ));
    }
    for target in &targets {
        plans.push(replace_plan(
            HARNESS_NATIVE_RENAME,
            target,
            Some(Arc::new(has_legacy_harness)),
            Arc::new(|input| {
                let (document, diagnostics) = harness_rename(input.target);
                Ok(TransformOutput {
                    document,
                    diagnostics,
                })
            }),
        ));
    }
    for target in &targets {
        plans.push(replace_plan(
            SUBSCRIPTION_PROVIDER_RENAME,
            target,
            Some(Arc::new(needs_provider_rename)),
            Arc::new(|input| {
                let (document, diagnostics) = provider_rename(input.target);
                Ok(TransformOutput {
                    document,
                    diagnostics,
                })
            }),
        ));
    }
    plans
}

/// The startup migration outcome.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StartupMigration {
    pub error: Option<String>,
    /// Sorted unique source paths moved by migrated plans.
    pub migrated_from: Vec<PathBuf>,
    pub journal_resumed: bool,
    pub plans: Vec<PlanResult>,
}

/// Runs the §12.2 plans once (not a dry run).
pub fn run_startup_migration(cwd: &Path, env: &Env, home: Option<&Path>) -> StartupMigration {
    run_startup_migration_with(cwd, env, home, MigrationEnv::default())
}

/// [`run_startup_migration`] with injectable process facts.
pub fn run_startup_migration_with(
    cwd: &Path,
    env: &Env,
    home: Option<&Path>,
    process: MigrationEnv,
) -> StartupMigration {
    let Some(home) = home.filter(|h| !h.as_os_str().is_empty()) else {
        return StartupMigration {
            error: Some("no home directory is available for the configuration migration".into()),
            ..StartupMigration::default()
        };
    };
    let discovery = Discovery {
        cwd: cwd.to_path_buf(),
        env: env.clone(),
        home: home.to_path_buf(),
    };
    let plans = startup_migration_plans(&discovery, &process.stamp());
    let batch = run_migrations(
        &plans,
        &BatchOptions {
            omo_dir: home.join(".omo"),
            dry_run: false,
            env: process,
        },
    );
    let mut migrated_from: Vec<PathBuf> = batch
        .plans
        .iter()
        .filter(|plan| plan.status == PlanStatus::Migrated)
        .flat_map(|plan| {
            plan.preview
                .iter()
                .flat_map(|p| p.moves.iter().map(|m| m.from.clone()))
        })
        .collect();
    migrated_from.sort();
    migrated_from.dedup();
    let error = if batch.status == BatchStatus::Locked {
        Some(
            "another process holds the configuration migration lock; migration is already running"
                .into(),
        )
    } else {
        batch.error.clone()
    };
    StartupMigration {
        error,
        migrated_from,
        journal_resumed: batch.journal_resumed,
        plans: batch.plans,
    }
}
