use std::cell::Cell;

use serde_json::json;

use super::*;
use crate::issue::PathSegment;
use crate::writer::types::OmoConfigWriteFileSystem;

#[test]
fn renewal_releases_guard_when_replacement_fails() {
    // Given
    let fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let clock = || 0;
    let alive = |_| true;
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock, env: &fixture.env, file_system: &fs, is_alive: &alive,
        lease_duration_ms: None, pid: 7,
    }).unwrap().unwrap();
    fs.fail_replace.set(true);
    // When
    let result = lock.renew();
    // Then
    assert!(matches!(result, Err(MigrationError::Fs(_))));
    assert!(!fs.exists(&mutation_guard_path(&fixture.env)));
}

#[test]
fn replacement_preplans_unused_backup_in_journal_and_run_result() {
    // Given
    let fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let base = format!("{}.bak.1970-01-01T00-00-00-000Z", fixture.target_path);
    fs.write(fixture.target_path, "{}").unwrap();
    fs.write(&base, "occupied").unwrap();
    fs.write(&format!("{base}.1"), "occupied").unwrap();
    let expected = format!("{base}.2");
    let plan = MigrationPlan {
        id: "planned".into(), mode: MigrationMode::ReplaceTarget, should_run: None,
        sources: vec![], target_path: fixture.target_path.into(),
        transform: Box::new(|_| Ok(json!({}).into())),
    };
    let writer = |input: MigrationTargetWriterInput<'_>| {
        let recorded = read_migration_journal(input.file_system, input.env)?.unwrap();
        assert_eq!(recorded.target_backup_path.as_deref(), Some(expected.as_str()));
        assert!(!recorded.target_written);
        assert_eq!(input.backup_path, Some(expected.as_str()));
        write_omo_migration_target(input)
    };
    // When
    let result = execute_plan(ExecutePlanInput {
        clock: &|| 0, dry_run: false, env: &fixture.env, file_system: &fs,
        journal_resumed: false, on_boundary: None, pid: 7, plan: &plan,
        renew_lock: &|| Ok(()), write_target: &writer,
    }).unwrap();
    // Then
    assert_eq!(result.target_backup_path, Some(expected.clone()));
    assert_eq!(fs.read(&expected).unwrap(), "{}");
    assert_eq!(fs.read(&base).unwrap(), "occupied");
}

#[test]
fn fresh_target_does_not_report_a_prewrite_backup() {
    // Given
    let fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    fs.write(fixture.source_path, "{}").unwrap();
    let plan = MigrationPlan {
        id: "fresh".into(), mode: MigrationMode::Merge, should_run: None,
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.into(), transform: Box::new(|_| Ok(json!({}).into())),
    };
    // When
    let result = execute_plan(ExecutePlanInput {
        clock: &|| 0, dry_run: false, env: &fixture.env, file_system: &fs,
        journal_resumed: false, on_boundary: None, pid: 7, plan: &plan,
        renew_lock: &|| Ok(()), write_target: &write_omo_migration_target,
    }).unwrap();
    // Then
    assert_eq!(result.target_backup_path, None);
    assert_eq!(result.status, MigrationStatus::Migrated);
}

#[test]
fn prepared_writer_receives_planned_backup_and_returns_actual_backup() {
    // Given
    let fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let prepared = PreparedTargetWrite { diagnostics: vec![], document: json!({}), edits: vec![] };
    let called = Cell::new(false);
    let writer = |input: MigrationTargetWriterInput<'_>| {
        assert_eq!(input.backup_path, Some("planned"));
        called.set(true);
        Ok(MigrationTargetWriteResult { backup_path: Some("actual".into()) })
    };
    // When
    let result = write_prepared_target(&fixture.env, &fs, &prepared, fixture.target_path, &writer, Some("planned")).unwrap();
    // Then
    assert!(called.get());
    assert_eq!(result.as_deref(), Some("actual"));
}

#[test]
fn replacement_edits_only_changed_nested_paths() {
    // Given
    let before = json!({"task":{"default_concurrency":3,"provider_concurrency":{"old":2,"keep":1}}});
    let after = json!({"task":{"default_concurrency":3,"provider_concurrency":{"keep":1,"new":4}}});
    // When
    let prepared = prepare_target_replacement(&after, "nested", &before, "/home/alice/.omo/omo.jsonc").unwrap();
    // Then
    assert_eq!(prepared.edits.len(), 3);
    assert_eq!(prepared.edits[0].path, vec![PathSegment::Key("task".into()), PathSegment::Key("provider_concurrency".into()), PathSegment::Key("old".into())]);
    assert_eq!(prepared.edits[0].value, None);
    assert_eq!(prepared.edits[1].value, Some(json!(4)));
}

fn journal(target_path: &str) -> MigrationJournal {
    MigrationJournal {
        backup_moves: vec![], completed_moves: vec![], diagnostics: vec![],
        migration_id: "resume".into(), target_backup_path: Some("/backup".into()),
        target_path: target_path.into(),
        target_write: MigrationJournalTargetWrite { additions: json!({}), mode: None },
        target_written: false, version: 1,
    }
}

#[test]
fn journal_round_trips_optional_backup_and_rejects_non_string() {
    // Given
    let fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let planned = journal(fixture.target_path);
    // When
    write_migration_journal(&planned, &fs, &fixture.env, 7, &|| 0).unwrap();
    let loaded = read_migration_journal(&fs, &fixture.env).unwrap().unwrap();
    let mut invalid = parse_file(&fs, &migration_journal_path(&fixture.env));
    invalid["targetBackupPath"] = json!(false);
    // Then
    assert_eq!(loaded, planned);
    assert!(parse_journal(&invalid).is_err());
    invalid.as_object_mut().unwrap().remove("targetBackupPath");
    assert_eq!(parse_journal(&invalid).unwrap().target_backup_path, None);
}

#[test]
fn resume_records_collision_replacement_before_injected_writer() {
    // Given
    let fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    fs.write(fixture.target_path, "{}").unwrap();
    fs.write("/backup", "foreign").unwrap();
    fs.write("/backup.1", "occupied").unwrap();
    write_migration_journal(&journal(fixture.target_path), &fs, &fixture.env, 7, &|| 0).unwrap();
    let writer = |input: MigrationTargetWriterInput<'_>| {
        let recorded = read_migration_journal(input.file_system, input.env)?.unwrap();
        assert_eq!(recorded.target_backup_path.as_deref(), Some("/backup.2"));
        assert_eq!(input.backup_path, Some("/backup.2"));
        write_omo_migration_target(input)
    };
    // When
    let result = resume_migration_journal(ResumeMigrationJournalInput {
        clock: &|| 0, env: &fixture.env, file_system: &fs, pid: 7,
        renew_lock: &|| Ok(()), write_target: &writer,
    }).unwrap();
    // Then
    assert!(result.resumed);
    assert_eq!(result.target_backup_path.as_deref(), Some("/backup.2"));
    assert_eq!(fs.read("/backup").unwrap(), "foreign");
    assert_eq!(fs.read("/backup.2").unwrap(), "{}");
}

#[test]
fn resumed_marked_target_reports_journal_backup_without_writing() {
    // Given
    let fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    fs.write(fixture.target_path, r#"{"_migrations":["resume"]}"#).unwrap();
    fs.write("/backup", "{}").unwrap();
    write_migration_journal(&journal(fixture.target_path), &fs, &fixture.env, 7, &|| 0).unwrap();
    // When
    let result = run_migrations(RunMigrationsOptions {
        after_migrations: None, clock: Some(&|| 0), discover: Box::new(Vec::new),
        dry_run: false, env: Some(fixture.env), file_system: Some(&fs),
        is_process_alive: Some(Box::new(|_| true)), lease_duration_ms: None,
        on_boundary: None, pid: Some(7), write_target: Some(&|_| panic!("marked target must not be written")),
    }).unwrap();
    // Then
    assert!(result.journal_resumed);
    assert_eq!(result.resumed_target_backup_path.as_deref(), Some("/backup"));
}

#[test]
fn current_process_is_alive_with_native_probe() {
    // Given
    let pid = std::process::id();
    // When
    let alive = default_is_process_alive(pid);
    // Then
    assert!(alive);
}

#[test]
#[cfg(any(unix, windows))]
fn impossible_process_is_dead_with_native_probe() {
    // Given
    let pid = u32::MAX;
    // When
    let alive = default_is_process_alive(pid);
    // Then
    assert!(!alive);
}
