use std::path::PathBuf;
use std::sync::{Arc, atomic::{AtomicU64, Ordering}};

pub fn create_reflection_run_id_factory(reflection: PathBuf, sessions: PathBuf, worktrees: PathBuf) -> Arc<dyn Fn() -> String + Send + Sync> {
    let high_water=Arc::new(AtomicU64::new(0));
    Arc::new(move || {
        let named=[reflection.join("completions"),reflection.join("runs"),sessions.clone(),worktrees.clone()].into_iter()
            .filter_map(|dir|std::fs::read_dir(dir).ok()).flatten().filter_map(|entry|entry.ok())
            .filter_map(|entry|run_number(&entry.file_name().to_string_lossy())).max().unwrap_or(0);
        let reserved=["active.lock","pending.json"].into_iter().filter_map(|name|std::fs::read(reflection.join(name)).ok())
            .filter_map(|bytes|serde_json::from_slice::<serde_json::Value>(&bytes).ok())
            .filter_map(|value|value.get("runId").and_then(serde_json::Value::as_str).and_then(run_number)).max().unwrap_or(0);
        let next=named.max(reserved).max(high_water.load(Ordering::Relaxed)).saturating_add(1);
        let _=high_water.fetch_update(Ordering::Relaxed,Ordering::Relaxed,|current|Some(current.max(next)));
        format!("reflection-run-{next}")
    })
}

fn run_number(name:&str)->Option<u64>{
    let name=name.rsplit('/').next().unwrap_or(name);
    let name=name.strip_suffix(".json").unwrap_or(name);
    let digits=name.rsplit_once("reflection-run-").map(|(_,tail)|tail)?;
    if digits.is_empty()||!digits.chars().all(|ch|ch.is_ascii_digit()){return None;}
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_advance_past_disk_and_reservation_state() {
        let root=tempfile::tempdir().unwrap();
        let reflection=root.path().join("reflection");let sessions=root.path().join("sessions");let worktrees=root.path().join("worktrees");
        for path in [&reflection.join("completions"),&sessions,&worktrees]{std::fs::create_dir_all(path).unwrap();}
        std::fs::write(reflection.join("completions/reflection-run-4.json"),"{}").unwrap();
        std::fs::write(reflection.join("active.lock"),r#"{"runId":"reflection-run-7"}"#).unwrap();
        std::fs::write(worktrees.join("1755000000000-reflection-run-9"),"").unwrap();
        let mint=create_reflection_run_id_factory(reflection,sessions,worktrees);
        assert_eq!(mint(),"reflection-run-10");assert_eq!(mint(),"reflection-run-11");
    }
}
