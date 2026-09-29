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
) -> Result<bool, MigrationError> {
    let journal = match read_migration_journal(input.file_system, input.env)? {
        Some(j) => j,
        None => return Ok(false),
    };

    (input.renew_lock)()?;
    let target = target_document(&journal.target_path, input.file_system)?;
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
        write_prepared_target(
            input.env,
            input.file_system,
            &prepared,
            &journal.target_path,
            input.write_target,
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
    Ok(true)
}
