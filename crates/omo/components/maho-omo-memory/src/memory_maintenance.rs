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
        let should_sweep = self.swept.lock().unwrap_or_else(|e| e.into_inner()).insert(context.identity.clone());
        if should_sweep { sweep_journals(&context, &self.options); }
        let options = self.options.clone();
        let scheduled = self.scheduled.clone();
        let tasks = self.tasks.clone();
        let identity = context.identity.clone();
        let task_identity = identity.clone();
        let handle = tokio::spawn(async move {
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
        let modified = modified.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        if now >= modified && now - modified > 10 * 60 * 1000 && fs::remove_dir_all(dir).is_ok() { removed += 1; }
    }
    if removed > 0 { if let Some(logger) = &options.logger { logger.info("omo-senpi memory removed empty transcript journals"); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    use memory_core::identity::layout::build_identity_paths;
    use crate::binding::create_memory_binding;
    use std::process::Command;

    #[derive(Clone, Default)]
    struct Logger { info: Arc<Mutex<Vec<String>>>, warn: Arc<Mutex<Vec<String>>> }
    impl MemoryMaintenanceLogger for Logger {
        fn info(&self, message: &str) { self.info.lock().unwrap().push(message.into()); }
        fn warn(&self, message: &str) { self.warn.lock().unwrap().push(message.into()); }
    }

    fn context(root: &Path) -> MemoryIdentityContext {
        let paths = build_identity_paths(root, "agent");
        MemoryIdentityContext::new("agent".into(), paths.clone(), create_memory_binding("agent", paths.repo.to_string_lossy().as_ref(), 1.0))
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git").args(args).current_dir(repo).output().unwrap();
        assert!(output.status.success(), "git {:?}: {}", args, String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn history(commits: usize) -> (tempfile::TempDir, MemoryIdentityContext) {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        fs::create_dir_all(&context.identity_paths.repo).unwrap();
        git(&context.identity_paths.repo, &["init", "-q"]);
        git(&context.identity_paths.repo, &["config", "user.email", "test@example.com"]);
        git(&context.identity_paths.repo, &["config", "user.name", "test"]);
        for index in 0..commits {
            fs::write(context.identity_paths.repo.join(format!("note-{index}.md")), format!("note {index}\n")).unwrap();
            git(&context.identity_paths.repo, &["add", "."]);
            git(&context.identity_paths.repo, &["commit", "-q", "-m", &format!("note {index}")]);
        }
        (root, context)
    }

    fn loose(repo: &Path) -> usize {
        git(repo, &["count-objects", "-v"]).lines().find_map(|line| line.strip_prefix("count: ").and_then(|v| v.parse().ok())).unwrap()
    }

    fn logger() -> Logger { Logger::default() }

    fn old_millis() -> u64 { u64::try_from(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis()).unwrap_or(u64::MAX) }

    fn empty_journal(context: &MemoryIdentityContext, name: &str, old: u64) -> std::path::PathBuf {
        let dir = context.identity_paths.transcripts.join(name);
        fs::create_dir_all(&dir).unwrap();
        let transcript = dir.join("transcript.jsonl");
        fs::write(&transcript, "").unwrap();
        let timestamp = format!("@{}", old / 1000);
        let status = Command::new("touch").args(["-d", &timestamp, transcript.to_str().unwrap(), dir.to_str().unwrap()]).status().unwrap();
        assert!(status.success());
        dir
    }

    #[tokio::test]
    async fn packs_loose_objects_and_keeps_commits_readable() {
        let (_root, context) = history(20); let log = logger(); let before = loose(&context.identity_paths.repo);
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { logger: Some(Arc::new(log.clone())), delay: Duration::ZERO, min_loose_objects: 10, ..Default::default() });
        maintenance.schedule(context.clone()); maintenance.settled().await;
        assert!(before >= 60); assert!(loose(&context.identity_paths.repo) < before); assert_eq!(*log.info.lock().unwrap(), vec!["omo-senpi memory repo packed"]); assert!(log.warn.lock().unwrap().is_empty()); assert_eq!(git(&context.identity_paths.repo, &["rev-list", "--count", "HEAD"]).trim(), "20");
    }

    #[tokio::test]
    async fn concurrent_commit_survives_pack() {
        let (_root, context) = history(20); let repo = context.identity_paths.repo.clone();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::from_millis(50), min_loose_objects: 10, ..Default::default() }); maintenance.schedule(context.clone());
        fs::write(repo.join("concurrent.md"), "written during maintenance\n").unwrap(); git(&repo, &["add", "."]); git(&repo, &["commit", "-q", "-m", "concurrent note"]); maintenance.settled().await;
        assert!(git(&repo, &["log", "--format=%s"]).lines().any(|line| line == "concurrent note")); assert!(!git(&repo, &["fsck", "--connectivity-only"]).contains("missing"));
    }

    #[tokio::test]
    async fn recent_pass_skips_second_process() {
        let (_root, context) = history(20); let first = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::ZERO, min_loose_objects: 10, ..Default::default() }); first.schedule(context.clone()); first.settled().await;
        for index in 20..40 { fs::write(context.identity_paths.repo.join(format!("note-{index}.md")), format!("note {index}\n")).unwrap(); git(&context.identity_paths.repo, &["add", "."]); git(&context.identity_paths.repo, &["commit", "-q", "-m", &format!("note {index}")]); }
        let before = loose(&context.identity_paths.repo); let log = logger(); let second = MemoryMaintenance::new(MemoryMaintenanceOptions { logger: Some(Arc::new(log.clone())), delay: Duration::ZERO, min_loose_objects: 10, ..Default::default() }); second.schedule(context.clone()); second.settled().await; assert_eq!(loose(&context.identity_paths.repo), before); assert!(log.info.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_repository_is_a_noop() {
        let root = tempfile::tempdir().unwrap();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::ZERO, now: Arc::new(|| 1_000), ..Default::default() });
        maintenance.schedule(context(root.path()));
        maintenance.settled().await;
    }

    #[tokio::test]
    async fn ten_runners_pack_once() { let (_root, context) = history(20); let log = logger(); let runners: Vec<_> = (0..10).map(|_| MemoryMaintenance::new(MemoryMaintenanceOptions { logger: Some(Arc::new(log.clone())), delay: Duration::ZERO, min_loose_objects: 10, ..Default::default() })).collect(); for runner in &runners { runner.schedule(context.clone()); } for runner in &runners { runner.settled().await; } assert_eq!(*log.info.lock().unwrap(), vec!["omo-senpi memory repo packed"]); assert!(log.warn.lock().unwrap().is_empty()); assert_eq!(loose(&context.identity_paths.repo), 0); }

    #[tokio::test]
    async fn held_lock_skips_without_warning() { let (_root, context) = history(20); fs::create_dir_all(&context.identity_paths.locks).unwrap(); fs::write(context.identity_paths.locks.join("memory-maintenance.lock"), "held").unwrap(); let before = loose(&context.identity_paths.repo); let log = logger(); let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { logger: Some(Arc::new(log.clone())), delay: Duration::ZERO, min_loose_objects: 10, ..Default::default() }); maintenance.schedule(context.clone()); maintenance.settled().await; assert_eq!(loose(&context.identity_paths.repo), before); assert!(log.info.lock().unwrap().is_empty()); assert!(log.warn.lock().unwrap().is_empty()); }

    #[tokio::test]
    async fn pending_loose_object_is_kept() { let (_root, context) = history(20); let pending_path = context.identity_paths.repo.join("pending.txt"); fs::write(&pending_path, "staged by a writer\n").unwrap(); let pending = git(&context.identity_paths.repo, &["hash-object", "-w", "pending.txt"]).trim().to_owned(); fs::remove_file(pending_path).unwrap(); let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::ZERO, min_loose_objects: 10, ..Default::default() }); maintenance.schedule(context.clone()); maintenance.settled().await; assert_eq!(git(&context.identity_paths.repo, &["cat-file", "-p", &pending]), "staged by a writer\n"); }

    #[tokio::test]
    async fn disposed_host_can_schedule_again() { let (_root, context) = history(20); let log = logger(); let host = MemoryMaintenance::new(MemoryMaintenanceOptions { logger: Some(Arc::new(log.clone())), delay: Duration::ZERO, min_loose_objects: 10, ..Default::default() }); host.schedule(context.clone()); host.dispose(); host.settled().await; host.schedule(context.clone()); host.settled().await; assert_eq!(*log.info.lock().unwrap(), vec!["omo-senpi memory repo packed"]); assert_eq!(loose(&context.identity_paths.repo), 0); }

    #[tokio::test]
    async fn stale_journals_removed_live_kept() { let root = tempfile::tempdir().unwrap(); let context = context(root.path()); let old = old_millis(); let stale = empty_journal(&context, "stale-control", old); let live = empty_journal(&context, "live-control", old); let log = logger(); let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { logger: Some(Arc::new(log.clone())), delay: Duration::ZERO, now: Arc::new(|| old_millis() + 60 * 60 * 1000), is_live_session: Arc::new(|id| id == "live-control"), ..Default::default() }); maintenance.schedule(context); maintenance.settled().await; assert!(!stale.exists()); assert!(live.exists()); assert_eq!(*log.info.lock().unwrap(), vec!["omo-senpi memory removed empty transcript journals"]); assert!(log.warn.lock().unwrap().is_empty()); }

    #[tokio::test]
    async fn dispose_still_sweeps_journal() { let root = tempfile::tempdir().unwrap(); let context = context(root.path()); let dir = empty_journal(&context, "health-check", old_millis()); let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::from_secs(60), now: Arc::new(|| old_millis() + 60 * 60 * 1000), ..Default::default() }); maintenance.schedule(context); maintenance.dispose(); maintenance.settled().await; assert!(!dir.exists()); }

    #[tokio::test]
    async fn sweep_runs_once_per_identity() { let root = tempfile::tempdir().unwrap(); let context = context(root.path()); let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::from_secs(60), now: Arc::new(|| old_millis() + 60 * 60 * 1000), ..Default::default() }); maintenance.schedule(context.clone()); maintenance.dispose(); maintenance.settled().await; let dir = empty_journal(&context, "later-empty", old_millis()); maintenance.schedule(context); maintenance.dispose(); maintenance.settled().await; assert!(dir.exists()); }

    #[tokio::test]
    async fn dispose_cancels_delayed_pass() {
        let root = tempfile::tempdir().unwrap();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions { delay: Duration::from_secs(60), ..Default::default() });
        maintenance.schedule(context(root.path()));
        maintenance.dispose();
        maintenance.settled().await;
    }

}

