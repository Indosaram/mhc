use crate::migration::types::{MigrationError, MigrationFileSystem};
use crate::writer::types::FsErrorKind;

pub fn move_migration_backup(
    file_system: &dyn MigrationFileSystem,
    source_path: &str,
    backup_path: &str,
) -> Result<(), MigrationError> {
    match file_system.rename(source_path, backup_path) {
        Ok(()) => Ok(()),
        Err(error) => {
            if error.kind != FsErrorKind::CrossDevice {
                return Err(MigrationError::Fs(error.message));
            }
            file_system.copy(source_path, backup_path)?;
            file_system.unlink(source_path)?;
            Ok(())
        }
    }
}
