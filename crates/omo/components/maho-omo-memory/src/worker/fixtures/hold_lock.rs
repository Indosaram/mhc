use std::{io::Write, path::Path};
pub async fn run(path: &Path) -> Result<(), String> {
    let record = memory_core::locks::create_lock_record("facts-finalize", memory_core::locks::CreateLockRecordOptions { run_id: Some("hold-lock-fixture".into()) }).map_err(|error| error.to_string())?;
    memory_core::locks::acquire_lock(path, &record, &memory_core::locks::AcquireLockOptions {
        wait_timeout_ms: Some(10_000), retry_delay_ms: Some(10), cancellation: None,
    }).map_err(|error| error.to_string())?;
    let mut output = std::io::stdout().lock();
    writeln!(output, "held").and_then(|()| output.flush()).map_err(|error| error.to_string())?;
    drop(output);
    std::future::pending::<Result<(), String>>().await
}
