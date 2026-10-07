use std::cell::Cell;

use omo_config_core::internal::posix_path::posix_dirname;
use omo_config_core::migration::*;
use omo_config_core::writer::types::{FsError, OmoConfigWriteFileSystem};
use serde_json::{Value, json};

#[test]
fn predicate_no_legacy_sources_does_not_trigger_migration() {
    let result = should_run_migration(&ShouldRunMigrationInput {
        legacy_sources_exist: false,
        migration_id: "legacy-a",
        target: &json!({}),
    });
    assert!(!result);
}

#[test]
fn predicate_existing_marker_prevents_migration() {
    let result = should_run_migration(&ShouldRunMigrationInput {
        legacy_sources_exist: true,
        migration_id: "legacy-a",
        target: &json!({ "_migrations": ["legacy-a"] }),
    });
    assert!(!result);
}

#[test]
fn predicate_distinct_marker_allows_migration() {
    let result = should_run_migration(&ShouldRunMigrationInput {
        legacy_sources_exist: true,
        migration_id: "legacy-b",
        target: &json!({ "_migrations": ["legacy-a"] }),
    });
    assert!(result);
}

#[test]
fn lock_expired_dead_process_takeover() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let path = migration_lock_path(&fixture.env);
    file_system.mkdirs(&posix_dirname(&path)).unwrap();
    file_system.write(&path, &lease_content(99, 1, 0)).unwrap();

    let clock = || 10i64;
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock,
        env: &fixture.env,
        file_system: &file_system,
        is_alive: &|_| false,
        lease_duration_ms: None,
        pid: 100,
    })
    .expect("acquire lock")
    .expect("expected lock takeover");

    let content = file_system.read(&path).unwrap();
    assert!(content.contains("\"pid\":100"));
    assert!(
        file_system
            .operations
            .borrow()
            .contains(&format!("remove:{path}"))
    );
    lock.release().expect("release");
}

#[test]
fn lock_fresh_live_process_not_displaced() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let path = migration_lock_path(&fixture.env);
    file_system.mkdirs(&posix_dirname(&path)).unwrap();
    let content = lease_content(99, 100, 0);
    file_system.write(&path, &content).unwrap();

    let clock = || 50i64;
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock,
        env: &fixture.env,
        file_system: &file_system,
        is_alive: &|_| true,
        lease_duration_ms: None,
        pid: 100,
    })
    .expect("acquire lock");

    assert!(lock.is_none());
    assert_eq!(file_system.read(&path).unwrap(), content);
}

#[test]
fn lock_live_owner_just_expired_safety_margin() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let path = migration_lock_path(&fixture.env);
    file_system.mkdirs(&posix_dirname(&path)).unwrap();
    let content = lease_content(99, 99, 0);
    file_system.write(&path, &content).unwrap();

    let clock = || 100i64;
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock,
        env: &fixture.env,
        file_system: &file_system,
        is_alive: &|_| true,
        lease_duration_ms: Some(10),
        pid: 100,
    })
    .expect("acquire lock");

    assert!(lock.is_none());
    assert_eq!(file_system.read(&path).unwrap(), content);
}

#[test]
fn lock_renew_updates_timestamp_before_release() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let path = migration_lock_path(&fixture.env);
    let now = Cell::new(10i64);
    let clock = || now.get();
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock,
        env: &fixture.env,
        file_system: &file_system,
        is_alive: &|_| true,
        lease_duration_ms: Some(5),
        pid: 100,
    })
    .expect("acquire lock")
    .expect("lock acquired");

    now.set(20);
    lock.renew().expect("renew");

    let content = file_system.read(&path).unwrap();
    assert!(content.contains("\"leaseExpiresAt\":25"));
    assert!(
        file_system
            .operations
            .borrow()
            .contains(&format!("replace:{path}"))
    );

    lock.release().expect("release");
    assert!(!file_system.exists(&path));
}

struct ReleaseContentionFileSystem {
    inner: MemoryMigrationFileSystem,
    release_guard_after_read: bool,
    guard_write_attempts: Cell<usize>,
}

impl ReleaseContentionFileSystem {
    fn new(release_guard_after_read: bool) -> Self {
        Self {
            inner: MemoryMigrationFileSystem::new(),
            release_guard_after_read,
            guard_write_attempts: Cell::new(0),
        }
    }
}

impl OmoConfigWriteFileSystem for ReleaseContentionFileSystem {
    fn copy(&self, source: &str, destination: &str) -> Result<(), FsError> {
        self.inner.copy(source, destination)
    }

    fn exists(&self, path: &str) -> bool {
        self.inner.exists(path)
    }

    fn is_symbolic_link(&self, path: &str) -> Result<bool, FsError> {
        self.inner.is_symbolic_link(path)
    }

    fn mkdirs(&self, path: &str) -> Result<(), FsError> {
        self.inner.mkdirs(path)
    }

    fn read(&self, path: &str) -> Result<String, FsError> {
        let content = self.inner.read(path)?;
        if self.release_guard_after_read
            && path.ends_with(".migration.lock.guard")
            && content.contains("\"pid\":99")
        {
            let _ = self.inner.unlink(path);
        }
        Ok(content)
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        self.inner.list_dir(path)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        self.inner.rename(from, to)
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        self.inner.unlink(path)
    }

    fn write_exclusive(&self, path: &str, content: &str) -> Result<(), FsError> {
        if path.ends_with(".migration.lock.guard") {
            let attempts = self.guard_write_attempts.get() + 1;
            self.guard_write_attempts.set(attempts);
            if !self.release_guard_after_read || attempts == 1 {
                let stale = format!("{}\n", json!({ "leaseExpiresAt": 1_000, "pid": 99 }));
                let _ = self.inner.write(path, &stale);
                return Err(FsError::exists(format!("Already exists {path}")));
            }
        }
        self.inner.write_exclusive(path, content)
    }

    fn write(&self, path: &str, content: &str) -> Result<(), FsError> {
        self.inner.write(path, content)
    }
}

impl MigrationFileSystem for ReleaseContentionFileSystem {
    fn replace_if_contents_match(
        &self,
        path: &str,
        expected: &str,
        content: &str,
    ) -> Result<bool, FsError> {
        self.inner
            .replace_if_contents_match(path, expected, content)
    }

    fn remove_if_contents_match(&self, path: &str, expected: &str) -> Result<bool, FsError> {
        self.inner.remove_if_contents_match(path, expected)
    }
}

#[test]
fn lock_transient_mutation_guard_contention_retries_and_removes_lock() {
    let file_system = ReleaseContentionFileSystem::new(true);
    let fixture = migration_fixture();
    let lock_path = migration_lock_path(&fixture.env);
    let clock = || 1000i64;
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock,
        env: &fixture.env,
        file_system: &file_system,
        is_alive: &|pid| pid == 99,
        lease_duration_ms: None,
        pid: 100,
    })
    .expect("acquire lock")
    .expect("lock acquired");

    lock.release().expect("release");
    assert!(file_system.guard_write_attempts.get() > 1);
    assert!(!file_system.exists(&lock_path));
}

#[test]
fn lock_persistent_mutation_guard_contention_release_fallback() {
    let file_system = ReleaseContentionFileSystem::new(false);
    let fixture = migration_fixture();
    let lock_path = migration_lock_path(&fixture.env);
    let clock = || 1000i64;
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock,
        env: &fixture.env,
        file_system: &file_system,
        is_alive: &|pid| pid == 99,
        lease_duration_ms: None,
        pid: 100,
    })
    .expect("acquire lock")
    .expect("lock acquired");

    lock.release().expect("release");
    assert_eq!(file_system.guard_write_attempts.get(), 6);
    assert!(!file_system.exists(&lock_path));
}

#[test]
fn lock_stale_mutation_guard_takeover_and_release() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let path = migration_lock_path(&fixture.env);
    let guard_path = format!("{path}.guard");
    let stale_content = lease_content(99, 1, 0);
    file_system.mkdirs(&posix_dirname(&path)).unwrap();
    file_system.write(&path, &stale_content).unwrap();
    file_system.write(&guard_path, &stale_content).unwrap();

    let clock = || 100_000i64;
    let lock = acquire_migration_lock(AcquireMigrationLockInput {
        clock: &clock,
        env: &fixture.env,
        file_system: &file_system,
        is_alive: &|pid| pid == 99,
        lease_duration_ms: None,
        pid: 100,
    })
    .expect("acquire lock")
    .expect("stale guard takeover");

    file_system.write(&guard_path, &stale_content).unwrap();
    lock.release().expect("release");

    assert!(!file_system.exists(&path));
    assert!(!file_system.exists(&guard_path));
}

#[test]
fn merge_commit_colliding_value_keeps_target_with_diagnostic() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    file_system
        .write(
            fixture.source_path,
            &json!({ "task": { "default_concurrency": 3 } }).to_string(),
        )
        .unwrap();
    file_system
        .write(
            fixture.target_path,
            &json!({ "task": { "default_concurrency": 5 } }).to_string(),
        )
        .unwrap();

    let result = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "legacy-task".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: None,
    })
    .expect("run migration");

    assert_eq!(result.status, MigrationStatus::Migrated);
    assert_eq!(
        result.diagnostics,
        vec!["skipped: task.default_concurrency legacy=3 kept=5"]
    );
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["task"]["default_concurrency"], 5);
}

#[test]
fn merge_commit_user_marker_does_not_suppress_project_target() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let project_target = "/work/project/.omo/omo.jsonc";
    file_system
        .write(
            fixture.source_path,
            &json!({ "task": { "default_concurrency": 3 } }).to_string(),
        )
        .unwrap();
    file_system
        .write(
            fixture.target_path,
            &json!({ "_migrations": ["legacy-task"] }).to_string(),
        )
        .unwrap();

    let result = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "legacy-task".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: project_target.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: None,
    })
    .expect("run migration");

    assert_eq!(result.status, MigrationStatus::Migrated);
    let proj = parse_file(&file_system, project_target);
    assert_eq!(proj["_migrations"], json!(["legacy-task"]));
    let user = parse_file(&file_system, fixture.target_path);
    assert_eq!(user["_migrations"], json!(["legacy-task"]));
}

#[test]
fn merge_commit_distinct_migration_ids_accumulate() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let source_a = "/legacy/a.jsonc";
    let source_b = "/legacy/b.jsonc";
    let transform_calls = Cell::new(0usize);
    file_system.write(source_a, "{}").unwrap();
    file_system.write(source_b, "{}").unwrap();

    let first = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "legacy-a".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(source_a)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| {
            transform_calls.set(transform_calls.get() + 1);
            Ok(json!({ "task": { "default_concurrency": 3 } }).into())
        }),
        write_target: None,
    })
    .expect("run migration 1");

    let second = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "legacy-b".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(source_b)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| {
            transform_calls.set(transform_calls.get() + 1);
            Ok(json!({ "task": { "wait": { "default_ms": 1000 } } }).into())
        }),
        write_target: None,
    })
    .expect("run migration 2");

    file_system.write(source_a, "{}").unwrap();
    let retry = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "legacy-a".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(source_a)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| {
            transform_calls.set(transform_calls.get() + 1);
            Ok(json!({ "task": { "default_concurrency": 3 } }).into())
        }),
        write_target: None,
    })
    .expect("run migration retry");

    assert_eq!(first.status, MigrationStatus::Migrated);
    assert_eq!(second.status, MigrationStatus::Migrated);
    assert_eq!(retry.status, MigrationStatus::Skipped);
    assert_eq!(transform_calls.get(), 2);
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["_migrations"], json!(["legacy-a", "legacy-b"]));
}

#[test]
fn merge_commit_rejected_by_schema_leaves_source_and_target_untouched() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    file_system.write(fixture.source_path, "{}").unwrap();
    file_system
        .write(
            fixture.target_path,
            &json!({ "task": { "default_concurrency": 5 } }).to_string(),
        )
        .unwrap();
    let target_before = file_system.read(fixture.target_path).unwrap();

    let err = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "invalid-config".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "unsupported_key": true }).into())),
        write_target: None,
    })
    .unwrap_err();

    assert!(err.to_string().contains("Migration validation failed"));
    assert_eq!(
        file_system.read(fixture.target_path).unwrap(),
        target_before
    );
    assert!(file_system.exists(fixture.source_path));
    assert!(!file_system.exists("/home/alice/.maho/.migration-journal.json"));
}

#[test]
fn in_place_crash_after_journal_resumes_cleanly() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    file_system
        .write(
            fixture.target_path,
            &json!({ "categories": { "deep": { "variant": "high" } } }).to_string(),
        )
        .unwrap();

    let err = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "replace-target-test".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::ReplaceTarget,
        on_boundary: Some(Box::new(|boundary| {
            if boundary == MigrationBoundary::JournalWritten {
                Err(MigrationError::crash("crash"))
            } else {
                Ok(())
            }
        })),
        pid: None,
        sources: vec![],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| {
            Ok(json!({ "categories": { "deep": { "reasoning": "high" } } }).into())
        }),
        write_target: None,
    })
    .unwrap_err();

    assert!(err.to_string().contains("crash"));

    let resumed = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "replace-target-test".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::ReplaceTarget,
        on_boundary: None,
        pid: None,
        sources: vec![],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| {
            Ok(json!({ "categories": { "deep": { "reasoning": "high" } } }).into())
        }),
        write_target: None,
    })
    .expect("resumed migration");

    assert!(resumed.journal_resumed);
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(
        target,
        json!({
            "categories": { "deep": { "reasoning": "high" } },
            "_migrations": ["replace-target-test"],
        })
    );
}

#[test]
fn batch_stale_live_owner_reclaimed_and_completed() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let lock_path = "/home/alice/.maho/.migration.lock";
    file_system.mkdirs(&posix_dirname(lock_path)).unwrap();
    file_system
        .write(lock_path, &lease_content(99, 1, 0))
        .unwrap();
    file_system.write(fixture.source_path, "{}").unwrap();

    let clock = || 100_000i64;
    let target_path_clone = fixture.target_path.to_string();
    let source_path_clone = fixture.source_path.to_string();

    let result = run_migrations(RunMigrationsOptions {
        after_migrations: None,
        clock: Some(&clock),
        discover: Box::new(move || {
            vec![MigrationPlan {
                id: "stale-live-owner".to_string(),
                mode: MigrationMode::Merge,
                should_run: None,
                sources: vec![MigrationSourceDescriptor::new(&source_path_clone)],
                target_path: target_path_clone.clone(),
                transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
            }]
        }),
        dry_run: false,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        is_process_alive: Some(Box::new(|pid| pid == 99)),
        lease_duration_ms: None,
        on_boundary: None,
        pid: Some(100),
        write_target: None,
    })
    .expect("run migrations");

    assert_eq!(result.status, MigrationBatchStatus::Completed);
    assert_eq!(
        result.results.iter().map(|e| e.status).collect::<Vec<_>>(),
        vec![MigrationStatus::Migrated]
    );
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["_migrations"], json!(["stale-live-owner"]));
    assert!(!file_system.exists(lock_path));
}

#[test]
fn batch_recovery_finishes_before_discovery_and_dry_run_prevents_writes() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let backup_path = format!("{}.backup", fixture.source_path);
    file_system.write(fixture.source_path, "{}").unwrap();

    let journal_path = migration_journal_path(&fixture.env);
    file_system.mkdirs(&posix_dirname(&journal_path)).unwrap();
    let journal_content = json!({
        "backupMoves": [{ "from": fixture.source_path, "to": backup_path }],
        "completedMoves": [],
        "migrationId": "recovery",
        "targetPath": fixture.target_path,
        "targetWrite": { "additions": { "task": { "default_concurrency": 3 } } },
        "targetWritten": false,
        "version": 1,
    });
    file_system
        .write(&journal_path, &format!("{journal_content}\n"))
        .unwrap();

    let discovery_after_recovery = Cell::new(false);
    let backup_path_clone = backup_path.clone();
    let target_path_clone = fixture.target_path.to_string();

    let result = run_migrations(RunMigrationsOptions {
        after_migrations: None,
        clock: None,
        discover: Box::new(|| {
            discovery_after_recovery.set(
                file_system.exists(&backup_path_clone)
                    && !file_system.exists(&migration_journal_path(&fixture.env)),
            );
            vec![MigrationPlan {
                id: "preview".to_string(),
                mode: MigrationMode::Merge,
                should_run: None,
                sources: vec![MigrationSourceDescriptor::new("/legacy/preview.jsonc")],
                target_path: target_path_clone.clone(),
                transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 4 } }).into())),
            }]
        }),
        dry_run: true,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        is_process_alive: Some(Box::new(|_| false)),
        lease_duration_ms: None,
        on_boundary: None,
        pid: Some(100),
        write_target: None,
    })
    .expect("run migrations");

    assert!(discovery_after_recovery.get());
    assert!(result.journal_resumed);
    assert_eq!(result.results[0].status, MigrationStatus::Skipped);
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["task"]["default_concurrency"], 3);
    assert!(!file_system.exists("/legacy/preview.jsonc"));
}

#[test]
fn recovery_crash_after_journal_written() {
    test_crash_recovery(MigrationBoundary::JournalWritten);
}

#[test]
fn recovery_crash_after_target_written() {
    test_crash_recovery(MigrationBoundary::TargetWritten);
}

#[test]
fn recovery_crash_after_target_recorded() {
    test_crash_recovery(MigrationBoundary::TargetRecorded);
}

#[test]
fn recovery_crash_after_source_moved() {
    test_crash_recovery(MigrationBoundary::SourceMoved);
}

#[test]
fn recovery_crash_after_source_recorded() {
    test_crash_recovery(MigrationBoundary::SourceRecorded);
}

fn test_crash_recovery(boundary: MigrationBoundary) {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let recovery_transform_calls = Cell::new(0usize);
    file_system.write(fixture.source_path, "{}").unwrap();

    let err = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "crash-safe".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: Some(Box::new(move |current| {
            if current == boundary {
                Err(MigrationError::crash(format!(
                    "Injected crash after {}",
                    boundary.as_str()
                )))
            } else {
                Ok(())
            }
        })),
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: None,
    })
    .unwrap_err();

    assert!(
        err.to_string()
            .contains(&format!("Injected crash after {}", boundary.as_str()))
    );

    let recovered = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "crash-safe".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| {
            recovery_transform_calls.set(recovery_transform_calls.get() + 1);
            Ok(json!({ "task": { "default_concurrency": 3 } }).into())
        }),
        write_target: None,
    })
    .expect("recovered migration");

    assert_eq!(recovered.status, MigrationStatus::Skipped);
    assert!(recovered.journal_resumed);
    assert_eq!(recovery_transform_calls.get(), 0);
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["_migrations"], json!(["crash-safe"]));
    assert!(!file_system.exists(fixture.source_path));
    assert!(file_system.exists(&format!("{}.bak.crash-safe", fixture.source_path)));
    assert!(!file_system.exists("/home/alice/.maho/.migration-journal.json"));
}

#[test]
fn transaction_concurrent_callers_exclusive_lock_and_retry() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let concurrent_status = Cell::new(String::new());
    file_system.write(fixture.source_path, "{}").unwrap();

    let first = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "concurrent-legacy".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: Some(&|write_input| {
            let second_res = run_migration(RunMigrationOptions {
                clock: None,
                env: Some(fixture.env.clone()),
                file_system: Some(write_input.file_system),
                id: "concurrent-legacy".to_string(),
                is_process_alive: None,
                lease_duration_ms: None,
                mode: MigrationMode::Merge,
                on_boundary: None,
                pid: Some(200),
                sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
                target_path: fixture.target_path.to_string(),
                transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
                write_target: None,
            })
            .unwrap();
            concurrent_status.set(second_res.status.as_str().to_string());
            write_omo_migration_target(write_input)
        }),
    })
    .expect("first migration");

    let second = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "concurrent-legacy".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(200),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: None,
    })
    .expect("second migration");

    assert_eq!(first.status, MigrationStatus::Migrated);
    assert_eq!(concurrent_status.take(), "locked");
    assert_eq!(second.status, MigrationStatus::Skipped);
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["_migrations"], json!(["concurrent-legacy"]));
}

#[test]
fn transaction_live_owner_fresh_lease_not_stolen() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let lock_path = "/home/alice/.maho/.migration.lock";
    file_system.mkdirs(&posix_dirname(lock_path)).unwrap();
    let lock_content = lease_content(99, 2000, 0);
    file_system.write(lock_path, &lock_content).unwrap();
    file_system.write(fixture.source_path, "{}").unwrap();

    let clock = || 1000i64;
    let result = run_migration(RunMigrationOptions {
        clock: Some(&clock),
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "live-owner".to_string(),
        is_process_alive: Some(Box::new(|pid| pid == 99)),
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: None,
    })
    .expect("run migration");

    assert_eq!(result.status, MigrationStatus::Locked);
    assert_eq!(file_system.read(lock_path).unwrap(), lock_content);
    assert!(file_system.exists(fixture.source_path));
    assert!(!file_system.exists(fixture.target_path));
}

#[test]
fn transaction_renewing_owner_never_stolen() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    let now = Cell::new(0i64);
    let competing_statuses = std::cell::RefCell::new(Vec::new());
    file_system.write(fixture.source_path, "{}").unwrap();

    let clock = || now.get();
    let race = || {
        let clock = || now.get();
        let res = run_migration(RunMigrationOptions {
            clock: Some(&clock),
            env: Some(fixture.env.clone()),
            file_system: Some(&file_system),
            id: "renewing-owner".to_string(),
            is_process_alive: Some(Box::new(|_| true)),
            lease_duration_ms: Some(10),
            mode: MigrationMode::Merge,
            on_boundary: None,
            pid: Some(200),
            sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
            target_path: fixture.target_path.to_string(),
            transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
            write_target: None,
        })
        .unwrap();
        competing_statuses
            .borrow_mut()
            .push(res.status.as_str().to_string());
    };

    let result = run_migration(RunMigrationOptions {
        clock: Some(&clock),
        env: Some(fixture.env.clone()),
        file_system: Some(&file_system),
        id: "renewing-owner".to_string(),
        is_process_alive: Some(Box::new(|_| true)),
        lease_duration_ms: Some(10),
        mode: MigrationMode::Merge,
        on_boundary: Some(Box::new(|boundary| {
            if boundary == MigrationBoundary::JournalWritten {
                now.set(8);
                race();
            }
            if boundary == MigrationBoundary::SourceMoved {
                now.set(24);
                race();
            }
            Ok(())
        })),
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: Some(&|write_input| {
            now.set(16);
            race();
            write_omo_migration_target(write_input)
        }),
    })
    .expect("run migration");

    assert_eq!(result.status, MigrationStatus::Migrated);
    assert_eq!(
        competing_statuses.into_inner(),
        vec!["locked", "locked", "locked"]
    );
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["_migrations"], json!(["renewing-owner"]));
}

#[test]
fn transaction_false_predicate_and_no_journal_does_not_write() {
    let no_source_fs = MemoryMigrationFileSystem::new();
    let marked_fs = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();

    marked_fs.write(fixture.source_path, "{}").unwrap();
    marked_fs
        .write(
            fixture.target_path,
            &json!({ "_migrations": ["already-done"] }).to_string(),
        )
        .unwrap();

    let no_source = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&no_source_fs),
        id: "absent-source".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: None,
    })
    .expect("run no_source");

    let marked = run_migration(RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(&marked_fs),
        id: "already-done".to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode: MigrationMode::Merge,
        on_boundary: None,
        pid: Some(100),
        sources: vec![MigrationSourceDescriptor::new(fixture.source_path)],
        target_path: fixture.target_path.to_string(),
        transform: Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
        write_target: None,
    })
    .expect("run marked");

    assert_eq!(no_source.status, MigrationStatus::Skipped);
    assert_eq!(marked.status, MigrationStatus::Skipped);
    assert!(!no_source_fs.exists(fixture.target_path));
    assert!(marked_fs.exists(fixture.source_path));
    assert_eq!(
        marked_fs.read(fixture.target_path).unwrap(),
        json!({ "_migrations": ["already-done"] }).to_string()
    );
    assert!(!no_source_fs.exists("/home/alice/.maho/.migration-journal.json"));
    assert!(!marked_fs.exists("/home/alice/.maho/.migration-journal.json"));
}

/// Every harness block the schema accepts (canonical `[opencode]`, `[native]`, `[codex]` plus the legacy `[senpi]` spelling), listed explicitly so a dropped block shows up here.
const HARNESS_BLOCKS: [&str; 4] = ["[opencode]", "[native]", "[codex]", "[senpi]"];

/// Every location `strip_retired_codegraph` is expected to clean: root, each harness block, each profile, each profile harness block.
fn retired_codegraph_target() -> Value {
    json!({
        "codegraph": { "enabled": true },
        "[opencode]": { "codegraph": { "enabled": true } },
        "[native]": { "codegraph": { "enabled": true } },
        "[codex]": { "codegraph": { "enabled": true } },
        "[senpi]": { "codegraph": { "enabled": true } },
        "profiles": {
            "default": {
                "codegraph": { "enabled": true },
                "[opencode]": { "codegraph": { "enabled": true } },
                "[native]": { "codegraph": { "enabled": true } },
                "[codex]": { "codegraph": { "enabled": true } },
                "[senpi]": { "codegraph": { "enabled": true } },
            },
        },
    })
}

const RETIRED_CODEGRAPH_DIAGNOSTICS: [&str; 10] = [
    "removed: codegraph (retired configuration)",
    "removed: [opencode].codegraph (retired configuration)",
    "removed: [native].codegraph (retired configuration)",
    "removed: [codex].codegraph (retired configuration)",
    "removed: [senpi].codegraph (retired configuration)",
    "removed: profiles.default.codegraph (retired configuration)",
    "removed: profiles.default.[opencode].codegraph (retired configuration)",
    "removed: profiles.default.[native].codegraph (retired configuration)",
    "removed: profiles.default.[codex].codegraph (retired configuration)",
    "removed: profiles.default.[senpi].codegraph (retired configuration)",
];

/// Asserts each section still exists (the migration must strip codegraph, not the whole block) and no longer carries codegraph.
fn assert_codegraph_stripped_from_every_section(target: &Value) {
    for harness in HARNESS_BLOCKS {
        assert!(target[harness].is_object(), "{harness} survives as an object");
        assert!(target[harness].get("codegraph").is_none(), "{harness}");
    }
    let profile = &target["profiles"]["default"];
    assert!(profile.is_object(), "profiles.default survives as an object");
    assert!(profile.get("codegraph").is_none());
    for harness in HARNESS_BLOCKS {
        assert!(profile[harness].is_object(), "profiles.default.{harness}");
        assert!(profile[harness].get("codegraph").is_none(), "profiles.default.{harness}");
    }
}

fn retired_codegraph_cleanup_options<'a>(
    fixture: &'a MigrationFixture,
    file_system: &'a MemoryMigrationFileSystem,
    id: &str,
    mode: MigrationMode,
    sources: Vec<MigrationSourceDescriptor>,
    transform: Box<MigrationTransform<'a>>,
) -> RunMigrationOptions<'a> {
    RunMigrationOptions {
        clock: None,
        env: Some(fixture.env.clone()),
        file_system: Some(file_system),
        id: id.to_string(),
        is_process_alive: None,
        lease_duration_ms: None,
        mode,
        on_boundary: None,
        pid: Some(100),
        sources,
        target_path: fixture.target_path.to_string(),
        transform,
        write_target: None,
    }
}

#[test]
fn retired_codegraph_cleanup_strips_every_location_before_a_merge() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    file_system.write(fixture.source_path, "{}").unwrap();
    file_system
        .write(fixture.target_path, &retired_codegraph_target().to_string())
        .unwrap();

    let result = run_migration(retired_codegraph_cleanup_options(
        &fixture,
        &file_system,
        "legacy-codegraph",
        MigrationMode::Merge,
        vec![MigrationSourceDescriptor::new(fixture.source_path)],
        Box::new(|_| Ok(json!({ "task": { "default_concurrency": 3 } }).into())),
    ))
    .expect("run migration");

    assert_eq!(result.status, MigrationStatus::Migrated);
    assert_eq!(
        result.diagnostics,
        RETIRED_CODEGRAPH_DIAGNOSTICS.map(str::to_string).to_vec()
    );
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["task"]["default_concurrency"], json!(3));
    assert_eq!(target["_migrations"], json!(["legacy-codegraph"]));
    assert!(target.get("codegraph").is_none());
    assert_codegraph_stripped_from_every_section(&target);
}

#[test]
fn retired_codegraph_cleanup_strips_every_location_before_a_replacement() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    file_system
        .write(fixture.target_path, &retired_codegraph_target().to_string())
        .unwrap();

    let result = run_migration(retired_codegraph_cleanup_options(
        &fixture,
        &file_system,
        "retire-codegraph",
        MigrationMode::ReplaceTarget,
        vec![],
        Box::new(|loaded| {
            let current = loaded
                .first()
                .map(|source| source.value.clone())
                .unwrap_or(Value::Null);
            let mut document = current.as_object().cloned().unwrap_or_default();
            document.insert("task".to_string(), json!({ "default_concurrency": 4 }));
            Ok(Value::Object(document).into())
        }),
    ))
    .expect("run migration");

    assert_eq!(result.status, MigrationStatus::Migrated);
    // the transform passed the target through, so target and document cleanup see the same paths: each is reported once
    assert_eq!(
        result.diagnostics,
        RETIRED_CODEGRAPH_DIAGNOSTICS.map(str::to_string).to_vec()
    );
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(target["task"]["default_concurrency"], json!(4));
    assert_eq!(target["_migrations"], json!(["retire-codegraph"]));
    assert!(target.get("codegraph").is_none());
    assert_codegraph_stripped_from_every_section(&target);
}

#[test]
fn retired_codegraph_is_stripped_from_a_renamed_harness_block() {
    let file_system = MemoryMigrationFileSystem::new();
    let fixture = migration_fixture();
    file_system
        .write(
            fixture.target_path,
            &json!({
                "[senpi]": { "codegraph": { "enabled": true } },
                "profiles": { "default": { "[senpi]": { "codegraph": { "enabled": true } } } },
                "_migrations": ["earlier-merge"],
            })
            .to_string(),
        )
        .unwrap();

    let result = run_migration(retired_codegraph_cleanup_options(
        &fixture,
        &file_system,
        "harness-native-rename",
        MigrationMode::ReplaceTarget,
        vec![],
        Box::new(|loaded| {
            let value = loaded
                .first()
                .map(|source| source.value.clone())
                .unwrap_or(Value::Null);
            Ok(Value::Object(
                omo_config_core::canonicalize_legacy_harness_blocks(&value).document,
            )
            .into())
        }),
    ))
    .expect("run migration");

    assert_eq!(result.status, MigrationStatus::Migrated);
    assert_eq!(
        result.diagnostics,
        vec![
            "removed: [senpi].codegraph (retired configuration)".to_string(),
            "removed: profiles.default.[senpi].codegraph (retired configuration)".to_string(),
            "removed: [native].codegraph (retired configuration)".to_string(),
            "removed: profiles.default.[native].codegraph (retired configuration)".to_string(),
        ]
    );
    let target = parse_file(&file_system, fixture.target_path);
    assert_eq!(
        target,
        json!({
            "[native]": {},
            "profiles": { "default": { "[native]": {} } },
            "_migrations": ["earlier-merge", "harness-native-rename"],
        })
    );
}
