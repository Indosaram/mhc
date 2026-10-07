use std::rc::Rc;

use crate::migration::batch::run_migrations;
use crate::migration::types::{
    MigrationBatchStatus, MigrationError, MigrationPlan, MigrationRunResult, MigrationStatus,
    RunMigrationOptions, RunMigrationsOptions,
};

pub fn run_migration<'a>(
    options: RunMigrationOptions<'a>,
) -> Result<MigrationRunResult, MigrationError> {
    let id = options.id.clone();
    let mode = options.mode;
    let sources = options.sources.clone();
    let target_path = options.target_path.clone();
    let transform = Rc::new(options.transform);

    let batch_result = run_migrations(RunMigrationsOptions {
        after_migrations: None,
        clock: options.clock,
        discover: Box::new(move || {
            let t = transform.clone();
            vec![MigrationPlan {
                id: id.clone(),
                mode,
                should_run: None,
                sources: sources.clone(),
                target_path: target_path.clone(),
                transform: Box::new(move |s| t(s)),
            }]
        }),
        dry_run: false,
        env: options.env,
        file_system: options.file_system,
        is_process_alive: options.is_process_alive,
        lease_duration_ms: options.lease_duration_ms,
        on_boundary: options.on_boundary,
        pid: options.pid,
        write_target: options.write_target,
    })?;

    if batch_result.status == MigrationBatchStatus::Locked {
        return Ok(MigrationRunResult {
            diagnostics: Vec::new(),
            journal_resumed: false,
            preview: None,
            status: MigrationStatus::Locked,
        });
    }

    Ok(batch_result
        .results
        .into_iter()
        .next()
        .unwrap_or(MigrationRunResult {
            diagnostics: Vec::new(),
            journal_resumed: batch_result.journal_resumed,
            preview: None,
            status: MigrationStatus::Skipped,
        }))
}
