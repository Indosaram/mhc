//! Per-monitor overflow file: lines a paced persistent watch matched beyond what one wake carries.
//! The wake names this file, so no matched line is lost to the batch cap. It lives in the session's
//! terminal state dir (not the watch's own monitor state dir, which is removed when the watch ends),
//! and is bounded: past `MONITOR_OVERFLOW_FILE_MAX_BYTES` it rotates to one `.1` copy. Files untouched
//! for longer than a durable watch can live are swept at session start.
use crate::monitor_overflow::MonitorSpillResult;
use crate::shared::{DURABLE_MONITOR_EXPIRY_MS, MONITOR_OVERFLOW_FILE_MAX_BYTES};
use std::io::Write;
use std::path::{Path, PathBuf};

fn overflow_dir(terminal_dir: &Path) -> PathBuf {
    terminal_dir.join("overflow")
}
pub fn monitor_overflow_path(terminal_dir: &Path, monitor_id: &str) -> PathBuf {
    overflow_dir(terminal_dir).join(format!("{monitor_id}.log"))
}

fn create_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Append `lines` to the watch's overflow file. Errors on a filesystem failure.
pub fn append_monitor_overflow(
    terminal_dir: &Path,
    monitor_id: &str,
    lines: &[String],
) -> std::io::Result<MonitorSpillResult> {
    append_monitor_overflow_bounded(
        terminal_dir,
        monitor_id,
        lines,
        MONITOR_OVERFLOW_FILE_MAX_BYTES,
    )
}

pub fn append_monitor_overflow_bounded(
    terminal_dir: &Path,
    monitor_id: &str,
    lines: &[String],
    max_bytes: u64,
) -> std::io::Result<MonitorSpillResult> {
    let path = monitor_overflow_path(terminal_dir, monitor_id);
    create_dir(&overflow_dir(terminal_dir))?;
    let size = match std::fs::metadata(&path) {
        Ok(info) => info.len(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    let rotated = size >= max_bytes;
    if rotated {
        let mut backup = path.clone().into_os_string();
        backup.push(".1");
        std::fs::rename(&path, PathBuf::from(backup))?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path)?;
    file.write_all(format!("{}\n", lines.join("\n")).as_bytes())?;
    Ok(MonitorSpillResult {
        path: path.to_string_lossy().into_owned(),
        rotated,
    })
}

/// Remove overflow files (and rotated copies) not written for longer than a durable watch lives.
pub async fn sweep_monitor_overflow_files(
    terminal_dir: &Path,
    now_ms: f64,
    max_age_ms: f64,
) -> std::io::Result<usize> {
    let dir = overflow_dir(terminal_dir);
    tokio::task::spawn_blocking(move || {
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.ends_with(".log") && !name.ends_with(".log.1") {
                continue;
            }
            let path = entry.path();
            let Ok(info) = std::fs::metadata(&path) else {
                continue;
            };
            let Some(modified) = info
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            else {
                continue;
            };
            if now_ms - modified.as_secs_f64() * 1000.0 <= max_age_ms {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            removed += 1;
        }
        Ok(removed)
    })
    .await
    .map_err(std::io::Error::other)?
}
/// Upstream `sweepMonitorOverflowFiles(dir)` with its defaults.
pub async fn sweep_monitor_overflow_files_default(terminal_dir: &Path) -> std::io::Result<usize> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0);
    sweep_monitor_overflow_files(terminal_dir, now, DURABLE_MONITOR_EXPIRY_MS as f64).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn appends_then_rotates_to_a_single_backup_past_the_cap() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let first = append_monitor_overflow_bounded(
            dir.path(),
            "mon_1",
            &["a".to_owned(), "b".to_owned()],
            4,
        )?;
        assert!(!first.rotated);
        assert_eq!(std::fs::read_to_string(&first.path)?, "a\nb\n");
        let second = append_monitor_overflow_bounded(dir.path(), "mon_1", &["c".to_owned()], 4)?;
        assert!(second.rotated);
        assert_eq!(std::fs::read_to_string(&second.path)?, "c\n");
        assert_eq!(
            std::fs::read_to_string(format!("{}.1", second.path))?,
            "a\nb\n"
        );
        Ok(())
    }
    #[tokio::test]
    async fn sweep_removes_only_stale_log_files() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        append_monitor_overflow(dir.path(), "mon_1", &["x".to_owned()])?;
        std::fs::write(overflow_dir(dir.path()).join("keep.txt"), "k")?;
        assert_eq!(
            sweep_monitor_overflow_files(dir.path(), 0.0, 1000.0).await?,
            0
        );
        let far_future = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
            + 10_000.0;
        assert_eq!(
            sweep_monitor_overflow_files(dir.path(), far_future, 1000.0).await?,
            1
        );
        assert!(overflow_dir(dir.path()).join("keep.txt").exists());
        assert_eq!(
            sweep_monitor_overflow_files(&dir.path().join("none"), far_future, 1000.0).await?,
            0
        );
        Ok(())
    }
}
