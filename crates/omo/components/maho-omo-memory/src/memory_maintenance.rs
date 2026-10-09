//! Background packing and stale transcript cleanup for identity repositories.

use crate::context::MemoryIdentityContext;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STAMP_KEY: &str = "omo.maintenanceAt";
const DEFAULT_DELAY_MS: u64 = 30_000;
const DEFAULT_INTERVAL_MS: u64 = 12 * 60 * 60 * 1000;
const DEFAULT_MIN_LOOSE_OBJECTS: usize = 2_000;

pub trait MemoryMaintenanceLogger: Send + Sync {
    fn info(&self, message: &str);
    fn warn(&self, message: &str);
}

#[derive(Clone)]
pub struct MemoryMaintenanceOptions {
    pub logger: Option<Arc<dyn MemoryMaintenanceLogger>>,
    pub delay: Duration,
    pub interval: Duration,
    pub min_loose_objects: usize,
    pub timeout: Duration,
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
    pub is_live_session: Arc<dyn Fn(&str) -> bool + Send + Sync>,
}

impl Default for MemoryMaintenanceOptions {
    fn default() -> Self {
        Self {
            logger: None,
            delay: Duration::from_millis(DEFAULT_DELAY_MS),
            interval: Duration::from_millis(DEFAULT_INTERVAL_MS),
            min_loose_objects: DEFAULT_MIN_LOOSE_OBJECTS,
            timeout: Duration::from_secs(600),
            now: Arc::new(|| SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64),
            is_live_session: Arc::new(|_| false),
        }
    }
}

pub struct MemoryMaintenance {
    options: MemoryMaintenanceOptions,
    scheduled: Arc<Mutex<HashSet<String>>>,
    swept: Arc<Mutex<HashSet<String>>>,
    tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
}

impl MemoryMaintenance {
    pub fn new(options: MemoryMaintenanceOptions) -> Self {
        Self { options, scheduled: Arc::new(Mutex::new(HashSet::new())), swept: Arc::new(Mutex::new(HashSet::new())), tasks: Arc::new(Mutex::new(HashMap::new())) }
    }

    pub fn schedule(&self, context: MemoryIdentityContext) {
        if !self.scheduled.lock().unwrap_or_else(|e| e.into_inner()).insert(context.identity.clone()) { return; }
        let options = self.options.clone();
        let swept = self.swept.clone();
        let scheduled = self.scheduled.clone();
        let tasks = self.tasks.clone();
        let identity = context.identity.clone();
        let task_identity = identity.clone();
        let handle = tokio::spawn(async move {
            if swept.lock().unwrap_or_else(|e| e.into_inner()).insert(task_identity.clone()) { sweep_journals(&context, &options); }
            tokio::time::sleep(options.delay).await;
            if let Err(error) = maintain(&context, &options) {
                if let Some(logger) = &options.logger { logger.warn(&format!("omo-senpi memory repo maintenance failed: {error}")); }
            }
            scheduled.lock().unwrap_or_else(|e| e.into_inner()).remove(&task_identity);
            tasks.lock().unwrap_or_else(|e| e.into_inner()).remove(&task_identity);
        });
        self.tasks.lock().unwrap_or_else(|e| e.into_inner()).insert(identity, handle);
    }

    pub fn dispose(&self) {
        for (_, task) in self.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain() { task.abort(); }
        self.scheduled.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }

    pub async fn settled(&self) {
        let tasks = self.tasks.lock().unwrap_or_else(|e| e.into_inner()).drain().map(|(_, task)| task).collect::<Vec<_>>();
        for task in tasks { let _ = task.await; }
    }
}

fn maintain(context: &MemoryIdentityContext, options: &MemoryMaintenanceOptions) -> Result<(), String> {
    if !context.identity_paths.repo.exists() { return Ok(()); }
    let lock_path = context.identity_paths.locks.join("memory-maintenance.lock");
    fs::create_dir_all(&context.identity_paths.locks).map_err(|e| e.to_string())?;
    let lock = match OpenOptions::new().write(true).create_new(true).open(&lock_path) { Ok(file) => file, Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()), Err(error) => return Err(error.to_string()) };
    let result = maintain_locked(context, options);
    drop(lock);
    let _ = fs::remove_file(lock_path);
    result
}

fn maintain_locked(context: &MemoryIdentityContext, options: &MemoryMaintenanceOptions) -> Result<(), String> {
    let previous = run_git(&context.identity_paths.repo, &["config", "--get", STAMP_KEY], options.timeout).ok().and_then(|value| value.trim().parse::<u64>().ok());
    let now = (options.now)();
    if previous.is_some_and(|last| now.saturating_sub(last) < options.interval.as_millis() as u64) { return Ok(()); }
    run_git(&context.identity_paths.repo, &["config", STAMP_KEY, &now.to_string()], options.timeout)?;
    let count = run_git(&context.identity_paths.repo, &["count-objects", "-v"], options.timeout)?;
    let loose = count.lines().find_map(|line| line.strip_prefix("count: ").and_then(|v| v.parse::<usize>().ok())).unwrap_or(0);
    if loose < options.min_loose_objects { return Ok(()); }
    run_git(&context.identity_paths.repo, &["repack", "-d", "-l"], options.timeout)?;
    run_git(&context.identity_paths.repo, &["prune-packed"], options.timeout)?;
    if let Some(logger) = &options.logger { logger.info("omo-senpi memory repo packed"); }
    Ok(())
}

fn run_git(repo: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut command = std::process::Command::new("git");
    command.args(args).current_dir(repo);
    let output = command.output().map_err(|e| e.to_string())?;
    let _ = timeout;
    if !output.status.success() { return Err(String::from_utf8_lossy(&output.stderr).trim().to_string()); }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn sweep_journals(context: &MemoryIdentityContext, options: &MemoryMaintenanceOptions) {
    let Ok(entries) = fs::read_dir(&context.identity_paths.transcripts) else { return; };
    let now = (options.now)();
    let mut removed = 0;
    for entry in entries.flatten() {
        let session = entry.file_name().to_string_lossy().into_owned();
        if (options.is_live_session)(&session) { continue; }
        let dir = entry.path();
        let transcript = dir.join("transcript.jsonl");
        if !transcript.is_file() || fs::metadata(&transcript).ok().and_then(|m| m.len().checked_sub(0)).unwrap_or(1) != 0 { continue; }
        let Ok(modified) = fs::metadata(&dir).and_then(|m| m.modified()) else { continue; };
        let age = modified.elapsed().unwrap_or_default().as_millis() as u64;
        if now >= age && now - age > 10 * 60 * 1000 && fs::remove_dir_all(dir).is_ok() { removed += 1; }
    }
    if removed > 0 { if let Some(logger) = &options.logger { logger.info("omo-senpi memory removed empty transcript journals"); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::identity::layout::build_identity_paths;
    use crate::binding::create_memory_binding;

    fn context(root: &Path) -> MemoryIdentityContext {
        let paths = build_identity_paths(root, "agent");
        MemoryIdentityContext::new("agent".into(), paths.clone(), create_memory_binding("agent", paths.repo.to_string_lossy(), 1.0))
    }

    #[tokio::test]
    async fn missing_repository_is_a_noop() {
        let root = tempfile::tempdir().unwrap();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::ZERO, now: Arc::new(|| 1_000), ..Default::default() });
        maintenance.schedule(context(root.path()));
        maintenance.settled().await;
    }

    #[tokio::test]
    async fn dispose_cancels_delayed_pass() {
        let root = tempfile::tempdir().unwrap();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::from_secs(60), ..Default::default() });
        maintenance.schedule(context(root.path()));
        maintenance.dispose();
        maintenance.settled().await;
    }

    #[test]
    fn sweep_keeps_live_empty_journal() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        fs::create_dir_all(context.identity_paths.transcripts.join("live")).unwrap();
        fs::write(context.identity_paths.transcripts.join("live/transcript.jsonl"), "").unwrap();
        let options = MemoryMaintenanceOptions { is_live_session: Arc::new(|id| id == "live"), now: Arc::new(|| 1_000), ..Default::default() };
        sweep_journals(&context, &options);
        assert!(context.identity_paths.transcripts.join("live").exists());
    }
}

