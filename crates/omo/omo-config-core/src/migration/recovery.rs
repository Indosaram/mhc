use crate::migration::backup_move::move_migration_backup;
use crate::migration::commit::{
    prepare_target_replacement, prepare_target_write, target_document, write_prepared_target,
};
use crate::migration::journal::{
    read_migration_journal, remove_migration_journal, write_migration_journal,
};
use crate::migration::predicate::has_migration_marker;
use crate::migration::types::{
    MigrationClock, MigrationEnvironment, MigrationError, MigrationFileSystem,
    MigrationTargetWriter,
};

pub struct ResumeMigrationJournalInput<'a> {
    pub clock: &'a dyn MigrationClock,
    pub env: &'a MigrationEnvironment,
    pub file_system: &'a dyn MigrationFileSystem,
    pub pid: u32,
    pub renew_lock: &'a dyn Fn() -> Result<(), MigrationError>,
    pub write_target: &'a MigrationTargetWriter<'a>,
}

pub fn resume_migration_journal(
    input: ResumeMigrationJournalInput<'_>,
) -> Result<ResumeMigrationJournalResult, MigrationError> {
    let mut journal = match read_migration_journal(input.file_system, input.env)? {
        Some(j) => j,
        None => return Ok(ResumeMigrationJournalResult::default()),
    };

    (input.renew_lock)()?;
    let target = target_document(&journal.target_path, input.file_system)?;
    let mut written_backup_path = None;
    if !has_migration_marker(&target, &journal.migration_id) {
        let prepared = if journal.target_write.mode.as_deref() == Some("replace-target") {
            prepare_target_replacement(
                &journal.target_write.additions,
                &journal.migration_id,
                &target,
                &journal.target_path,
            )?
        } else {
            prepare_target_write(
                &journal.target_write.additions,
                &journal.migration_id,
                &target,
                &journal.target_path,
            )?
        };
        let backup_path = backup_path_for_resume(journal.target_backup_path.as_deref(), &journal.target_path, input.file_system)?;
        if backup_path != journal.target_backup_path && backup_path.is_some() {
            journal.target_backup_path = backup_path;
            write_migration_journal(&journal, input.file_system, input.env, input.pid, input.clock)?;
        }
        written_backup_path = write_prepared_target(
            input.env,
            input.file_system,
            &prepared,
            &journal.target_path,
            input.write_target,
            journal.target_backup_path.as_deref(),
        )?;
    }

    let mut target_recorded = journal;
    target_recorded.target_written = true;
    write_migration_journal(
        &target_recorded,
        input.file_system,
        input.env,
        input.pid,
        input.clock,
    )?;

    for m in &target_recorded.backup_moves.clone() {
        if target_recorded.completed_moves.contains(&m.from) {
            continue;
        }
        (input.renew_lock)()?;
        if input.file_system.exists(&m.from) {
            if input.file_system.exists(&m.to) {
                return Err(MigrationError::transaction(format!(
                    "Migration backup path already exists: {}",
                    m.to
                )));
            }
            move_migration_backup(input.file_system, &m.from, &m.to)?;
        } else if !input.file_system.exists(&m.to) {
            return Err(MigrationError::transaction(format!(
                "Migration source and backup are both missing: {}",
                m.from
            )));
        }
        target_recorded.completed_moves.push(m.from.clone());
        write_migration_journal(
            &target_recorded,
            input.file_system,
            input.env,
            input.pid,
            input.clock,
        )?;
    }

    remove_migration_journal(input.file_system, input.env)?;
    let target_backup_path = written_backup_path.or_else(|| {
        target_recorded.target_backup_path.filter(|path| input.file_system.exists(path))
    });
    Ok(ResumeMigrationJournalResult { resumed: true, target_backup_path })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResumeMigrationJournalResult {
    pub resumed: bool,
    pub target_backup_path: Option<String>,
}

fn backup_path_for_resume(
    planned: Option<&str>,
    target_path: &str,
    file_system: &dyn MigrationFileSystem,
) -> Result<Option<String>, MigrationError> {
    let Some(planned) = planned else { return Ok(None); };
    if !file_system.exists(planned) || !file_system.exists(target_path)
        || file_system.read(planned)? == file_system.read(target_path)?
    {
        return Ok(Some(planned.to_string()));
    }
    let mut attempt = 1usize;
    while file_system.exists(&format!("{planned}.{attempt}")) {
        attempt += 1;
    }
    Ok(Some(format!("{planned}.{attempt}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::migration::migration_test_support::MemoryMigrationFileSystem;
    use crate::writer::types::OmoConfigWriteFileSystem;

    #[test]
    fn resume_keeps_free_or_matching_backup_and_absent_plan() {
        // Given
        let fs = MemoryMigrationFileSystem::new();
        fs.write("/target", "original").unwrap();
        fs.write("/matching", "original").unwrap();
        // When
        let free = backup_path_for_resume(Some("/free"), "/target", &fs).unwrap();
        let matching = backup_path_for_resume(Some("/matching"), "/target", &fs).unwrap();
        let absent = backup_path_for_resume(None, "/target", &fs).unwrap();
        // Then
        assert_eq!(free.as_deref(), Some("/free"));
        assert_eq!(matching.as_deref(), Some("/matching"));
        assert_eq!(absent, None);
    }
}
