use std::collections::BTreeSet;

use serde_json::Value;

use crate::internal::jsonc::parse_jsonc_safe;
use crate::internal::plain_object::is_plain_object;
use crate::internal::posix_path::{posix_dirname, to_posix_path};
use crate::migration::backup_move::move_migration_backup;
use crate::migration::commit::{
    prepare_target_replacement, prepare_target_write, target_document, write_omo_migration_target,
    write_prepared_target,
};
use crate::migration::journal::{
    MigrationJournal, MigrationJournalTargetWrite, migration_journal_path,
    remove_migration_journal, write_migration_journal,
};
use crate::migration::lock::{
    AcquireMigrationLockInput, acquire_migration_lock, migration_lock_path,
};
use crate::migration::predicate::{ShouldRunMigrationInput, should_run_migration};
use crate::migration::recovery::{ResumeMigrationJournalInput, resume_migration_journal};
use crate::migration::types::{
    DefaultMigrationClock, LoadedMigrationSource, MigrationBackupMove, MigrationBatchRunResult,
    MigrationBatchStatus, MigrationBoundary, MigrationClock, MigrationEnvironment, MigrationError,
    MigrationFileSystem, MigrationMode, MigrationPlan, MigrationPreview, MigrationRunResult,
    MigrationSourceDescriptor, MigrationStatus, MigrationTargetWriter, RunMigrationsOptions,
    default_is_process_alive, default_migration_env, default_pid,
};
use crate::writer::types::StdWriteFileSystem;

fn parse_source(path: &str, content: &str) -> Result<Value, MigrationError> {
    let parsed = parse_jsonc_safe(content);
    if !parsed.errors.is_empty() || parsed.data.is_none() {
        let detail = parsed
            .errors
            .iter()
            .map(|error| format!("{} at {}", error.message, error.offset))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(MigrationError::transaction(format!(
            "Migration source at {path} is invalid JSONC: {detail}"
        )));
    }
    Ok(parsed.data.unwrap())
}

fn load_sources(
    sources: &[MigrationSourceDescriptor],
    file_system: &dyn MigrationFileSystem,
) -> Result<Vec<LoadedMigrationSource>, MigrationError> {
    let mut loaded = Vec::new();
    for source in sources {
        if file_system.exists(&source.path) {
            let content = file_system.read(&source.path)?;
            let value = parse_source(&source.path, &content)?;
            loaded.push(LoadedMigrationSource {
                backup_path: source.backup_path.clone(),
                path: source.path.clone(),
                value,
            });
        }
    }
    Ok(loaded)
}

fn encode_uri_component(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'a'..=b'z'
            | b'A'..=b'Z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn backup_base_path(source: &MigrationSourceDescriptor, migration_id: &str) -> String {
    source
        .backup_path
        .clone()
        .unwrap_or_else(|| format!("{}.bak.{}", source.path, encode_uri_component(migration_id)))
}

fn backup_moves(
    sources: &[MigrationSourceDescriptor],
    migration_id: &str,
    file_system: &dyn MigrationFileSystem,
    protected_paths: &BTreeSet<String>,
) -> Result<Vec<MigrationBackupMove>, MigrationError> {
    let paths: BTreeSet<String> = sources.iter().map(|s| s.path.clone()).collect();
    let mut destinations = BTreeSet::new();
    let mut moves = Vec::new();

    for source in sources {
        if !file_system.exists(&source.path) {
            continue;
        }
        let base_path = backup_base_path(source, migration_id);
        let mut destination = base_path.clone();
        let mut attempt = 1usize;
        while file_system.exists(&destination) || destinations.contains(&destination) {
            if source.backup_path.is_some() {
                return Err(MigrationError::transaction(format!(
                    "Migration backup path already exists: {destination}"
                )));
            }
            destination = format!("{base_path}.{attempt}");
            attempt += 1;
        }
        if paths.contains(&destination) || protected_paths.contains(&destination) {
            return Err(MigrationError::transaction(format!(
                "Migration backup path is protected: {destination}"
            )));
        }
        destinations.insert(destination.clone());
        moves.push(MigrationBackupMove {
            from: source.path.clone(),
            to: destination,
        });
    }

    Ok(moves)
}

fn assert_safe_source_paths(
    sources: &[MigrationSourceDescriptor],
    protected_paths: &BTreeSet<String>,
) -> Result<(), MigrationError> {
    let mut seen = BTreeSet::new();
    for source in sources {
        if seen.contains(&source.path) {
            return Err(MigrationError::transaction(format!(
                "Duplicate migration source: {}",
                source.path
            )));
        }
        if protected_paths.contains(&source.path) {
            return Err(MigrationError::transaction(format!(
                "Migration source is protected: {}",
                source.path
            )));
        }
        seen.insert(source.path.clone());
    }
    Ok(())
}

fn ensure_backup_directories(
    moves: &[MigrationBackupMove],
    file_system: &dyn MigrationFileSystem,
) -> Result<(), MigrationError> {
    for m in moves {
        let dir = posix_dirname(&to_posix_path(&m.to));
        file_system.mkdirs(&dir)?;
    }
    Ok(())
}

pub struct ExecutePlanInput<'a, 'p> {
    pub clock: &'a dyn MigrationClock,
    pub dry_run: bool,
    pub env: &'a MigrationEnvironment,
    pub file_system: &'a dyn MigrationFileSystem,
    pub journal_resumed: bool,
    pub on_boundary: Option<&'a (dyn Fn(MigrationBoundary) -> Result<(), MigrationError> + 'a)>,
    pub pid: u32,
    pub plan: &'p MigrationPlan<'p>,
    pub renew_lock: &'a dyn Fn() -> Result<(), MigrationError>,
    pub write_target: &'a MigrationTargetWriter<'a>,
}

pub fn execute_plan(input: ExecutePlanInput<'_, '_>) -> Result<MigrationRunResult, MigrationError> {
    let env = input.env;
    let file_system = input.file_system;
    let journal_resumed = input.journal_resumed;
    let plan = input.plan;

    let mut protected_paths = BTreeSet::new();
    protected_paths.insert(plan.target_path.clone());
    protected_paths.insert(migration_journal_path(env));
    protected_paths.insert(migration_lock_path(env));

    assert_safe_source_paths(&plan.sources, &protected_paths)?;

    let existing_sources: Vec<_> = plan
        .sources
        .iter()
        .filter(|source| file_system.exists(&source.path))
        .cloned()
        .collect();

    let target = target_document(&plan.target_path, file_system)?;
    let replace_target = plan.mode == MigrationMode::ReplaceTarget;
    let legacy_sources_exist = if replace_target {
        file_system.exists(&plan.target_path)
    } else {
        !existing_sources.is_empty()
    };

    if !should_run_migration(&ShouldRunMigrationInput {
        legacy_sources_exist,
        migration_id: &plan.id,
        target: &target,
    }) {
        return Ok(MigrationRunResult {
            diagnostics: Vec::new(),
            journal_resumed,
            preview: None,
            status: MigrationStatus::Skipped,
        });
    }

    let loaded = if replace_target {
        vec![LoadedMigrationSource {
            backup_path: None,
            path: plan.target_path.clone(),
            value: target.clone(),
        }]
    } else {
        load_sources(&existing_sources, file_system)?
    };

    let transformed = (plan.transform)(&loaded)?;
    if !is_plain_object(&transformed.document) {
        return Err(MigrationError::transaction(
            "Migration transform must return a plain object",
        ));
    }

    let prepared = if replace_target {
        prepare_target_replacement(&transformed.document, &plan.id, &target, &plan.target_path)?
    } else {
        prepare_target_write(&transformed.document, &plan.id, &target, &plan.target_path)?
    };

    let mut diagnostics = transformed.diagnostics;
    diagnostics.extend(prepared.diagnostics.clone());

    let moves = backup_moves(&existing_sources, &plan.id, file_system, &protected_paths)?;
    let preview = MigrationPreview {
        backup_moves: moves.clone(),
        target_path: plan.target_path.clone(),
        transform: transformed.document.clone(),
    };

    if input.dry_run {
        return Ok(MigrationRunResult {
            diagnostics,
            journal_resumed,
            preview: Some(preview),
            status: MigrationStatus::Planned,
        });
    }

    ensure_backup_directories(&moves, file_system)?;

    let journal = MigrationJournal {
        backup_moves: moves,
        completed_moves: Vec::new(),
        diagnostics: diagnostics.clone(),
        migration_id: plan.id.clone(),
        target_path: plan.target_path.clone(),
        target_write: MigrationJournalTargetWrite {
            additions: transformed.document,
            mode: if replace_target {
                Some("replace-target".to_string())
            } else {
                None
            },
        },
        target_written: false,
        version: 1,
    };

    write_migration_journal(&journal, file_system, env, input.pid, input.clock)?;
    if let Some(on_boundary) = input.on_boundary {
        on_boundary(MigrationBoundary::JournalWritten)?;
    }

    (input.renew_lock)()?;
    write_prepared_target(
        env,
        file_system,
        &prepared,
        &plan.target_path,
        input.write_target,
    )?;
    if let Some(on_boundary) = input.on_boundary {
        on_boundary(MigrationBoundary::TargetWritten)?;
    }

    let mut target_recorded = journal;
    target_recorded.target_written = true;
    write_migration_journal(&target_recorded, file_system, env, input.pid, input.clock)?;
    if let Some(on_boundary) = input.on_boundary {
        on_boundary(MigrationBoundary::TargetRecorded)?;
    }

    for m in &target_recorded.backup_moves.clone() {
        (input.renew_lock)()?;
        if file_system.exists(&m.to) {
            return Err(MigrationError::transaction(format!(
                "Migration backup path already exists: {}",
                m.to
            )));
        }
        move_migration_backup(file_system, &m.from, &m.to)?;
        if let Some(on_boundary) = input.on_boundary {
            on_boundary(MigrationBoundary::SourceMoved)?;
        }
        target_recorded.completed_moves.push(m.from.clone());
        write_migration_journal(&target_recorded, file_system, env, input.pid, input.clock)?;
        if let Some(on_boundary) = input.on_boundary {
            on_boundary(MigrationBoundary::SourceRecorded)?;
        }
    }

    remove_migration_journal(file_system, env)?;

    Ok(MigrationRunResult {
        diagnostics,
        journal_resumed,
        preview: Some(preview),
        status: MigrationStatus::Migrated,
    })
}

pub fn run_migrations(
    options: RunMigrationsOptions<'_>,
) -> Result<MigrationBatchRunResult, MigrationError> {
    let default_clock = DefaultMigrationClock;
    let clock: &dyn MigrationClock = options.clock.unwrap_or(&default_clock);
    let default_env = default_migration_env();
    let env: &MigrationEnvironment = options.env.as_ref().unwrap_or(&default_env);
    let default_fs = StdWriteFileSystem;
    let file_system: &dyn MigrationFileSystem = options.file_system.unwrap_or(&default_fs);
    let default_liveness = default_is_process_alive;
    let is_alive: Box<dyn Fn(u32) -> bool> = match options.is_process_alive {
        Some(f) => f,
        None => Box::new(default_liveness),
    };
    let pid = options.pid.unwrap_or_else(default_pid);
    let write_target_fn = write_omo_migration_target;
    let write_target = options.write_target.unwrap_or(&write_target_fn);

    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock,
        env,
        file_system,
        is_alive: &*is_alive,
        lease_duration_ms: options.lease_duration_ms,
        pid,
    })?;

    let lock = match lock {
        Some(l) => l,
        None => {
            return Ok(MigrationBatchRunResult {
                journal_resumed: false,
                results: Vec::new(),
                status: MigrationBatchStatus::Locked,
            });
        }
    };

    let renew_lock = || lock.renew();

    let journal_resumed = resume_migration_journal(ResumeMigrationJournalInput {
        clock,
        env,
        file_system,
        pid,
        renew_lock: &renew_lock,
        write_target,
    })?;

    renew_lock()?;

    let plans = (options.discover)();
    let mut results = Vec::new();
    for plan in &plans {
        let res = execute_plan(ExecutePlanInput {
            clock,
            dry_run: options.dry_run,
            env,
            file_system,
            journal_resumed,
            on_boundary: options.on_boundary.as_deref(),
            pid,
            plan,
            renew_lock: &renew_lock,
            write_target,
        })?;
        results.push(res);
    }

    if !options.dry_run
        && let Some(after) = options.after_migrations
    {
        after(&results);
    }

    lock.release()?;

    Ok(MigrationBatchRunResult {
        journal_resumed,
        results,
        status: MigrationBatchStatus::Completed,
    })
}
