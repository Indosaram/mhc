use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use memory_core::reflection::run_id_high_water::{
    REFLECTION_RUN_ID_PREFIX, RunIdHighWaterPaths, read_reflection_run_id_mark,
    record_reflection_run_id_mark, scan_reflection_run_high_water,
};

pub fn create_reflection_run_id_factory(
    reflection: PathBuf,
    sessions: PathBuf,
    worktrees: PathBuf,
) -> Arc<dyn Fn() -> String + Send + Sync> {
    let high_water = Arc::new(AtomicU64::new(0));
    let scan_paths = RunIdHighWaterPaths {
        reflection: reflection.clone(),
        reflection_sessions: sessions,
        worktrees,
    };
    Arc::new(move || {
        let scanned = scan_reflection_run_high_water(&scan_paths);
        let mark = read_reflection_run_id_mark(&reflection).unwrap_or(0);
        let next = scanned
            .max(mark)
            .max(high_water.load(Ordering::Relaxed))
            .saturating_add(1);
        let _ = high_water.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            Some(current.max(next))
        });
        let _ = record_reflection_run_id_mark(&reflection, next);
        format!("{REFLECTION_RUN_ID_PREFIX}{next}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_advance_past_disk_and_reservation_state() {
        let root = tempfile::tempdir().unwrap();
        let reflection = root.path().join("reflection");
        let sessions = root.path().join("sessions");
        let worktrees = root.path().join("worktrees");
        for path in [&reflection.join("completions"), &sessions, &worktrees] {
            std::fs::create_dir_all(path).unwrap();
        }
        std::fs::write(reflection.join("completions/reflection-run-4.json"), "{}").unwrap();
        std::fs::write(
            reflection.join("active.lock"),
            r#"{"runId":"reflection-run-7"}"#,
        )
        .unwrap();
        std::fs::write(worktrees.join("1755000000000-reflection-run-9"), "").unwrap();
        let mint = create_reflection_run_id_factory(
            reflection.clone(),
            sessions.clone(),
            worktrees.clone(),
        );
        assert_eq!(mint(), "reflection-run-10");
        assert_eq!(mint(), "reflection-run-11");

        // Even after retention removes every visible run trace, a fresh factory never reuses ids.
        std::fs::remove_file(reflection.join("completions/reflection-run-4.json")).unwrap();
        std::fs::remove_file(reflection.join("active.lock")).unwrap();
        std::fs::remove_file(worktrees.join("1755000000000-reflection-run-9")).unwrap();
        let fresh = create_reflection_run_id_factory(reflection, sessions, worktrees);
        assert_eq!(fresh(), "reflection-run-12");
    }
}
