use std::rc::Rc;

use omo_config_core::{
    MigrationBatchStatus, MigrationBoundaryHook, MigrationClock, MigrationEnvironment,
    MigrationError, MigrationFileSystem, MigrationPlan, MigrationRunResult, MigrationStatus,
    MigrationTargetWriter, MigrationTransformResult, RunMigrationsOptions, run_migrations,
};
use serde_json::Value;

use crate::migration_plans::LegacyConfigMigrationPlan;

#[derive(Default)]
pub struct ExecuteLegacyConfigMigrationPlanOptions<'a> {
    pub clock: Option<&'a dyn MigrationClock>,
    pub dry_run: bool,
    pub env: Option<MigrationEnvironment>,
    pub file_system: Option<&'a dyn MigrationFileSystem>,
    pub is_process_alive: Option<Box<dyn Fn(u32) -> bool + 'a>>,
    pub lease_duration_ms: Option<i64>,
    pub on_boundary: Option<Box<MigrationBoundaryHook<'a>>>,
    pub pid: Option<u32>,
    pub write_target: Option<&'a MigrationTargetWriter<'a>>,
}

pub fn execute_legacy_config_migration_plan(
    plan: &LegacyConfigMigrationPlan,
    options: ExecuteLegacyConfigMigrationPlanOptions<'_>,
) -> Result<MigrationRunResult, MigrationError> {
    let plan = plan.clone();
    let batch = run_migrations(RunMigrationsOptions {
        after_migrations: None,
        clock: options.clock,
        discover: Box::new(move || {
            let transform = Rc::clone(&plan.transform);
            vec![MigrationPlan {
                id: plan.id.clone(),
                mode: plan.mode,
                sources: plan.sources.clone(),
                target_path: plan.target_path.clone(),
                transform: Box::new(move |loaded| {
                    let result = transform(loaded)?;
                    Ok(MigrationTransformResult {
                        diagnostics: result.diagnostics,
                        document: Value::Object(result.document),
                    })
                }),
            }]
        }),
        dry_run: options.dry_run,
        env: options.env,
        file_system: options.file_system,
        is_process_alive: options.is_process_alive,
        lease_duration_ms: options.lease_duration_ms,
        on_boundary: options.on_boundary,
        pid: options.pid,
        write_target: options.write_target,
    })?;
    if batch.status == MigrationBatchStatus::Locked {
        return Ok(MigrationRunResult {
            diagnostics: Vec::new(),
            journal_resumed: false,
            preview: None,
            status: MigrationStatus::Locked,
        });
    }
    let journal_resumed = batch.journal_resumed;
    Ok(batch
        .results
        .into_iter()
        .next()
        .unwrap_or(MigrationRunResult {
            diagnostics: Vec::new(),
            journal_resumed,
            preview: None,
            status: MigrationStatus::Skipped,
        }))
}
