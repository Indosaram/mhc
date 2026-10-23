//! Background packing and stale transcript cleanup for identity repositories.

use crate::context::MemoryIdentityContext;
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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
    pub executable: Option<PathBuf>,
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
            executable: None,
            now: Arc::new(|| {
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64
            }),
            is_live_session: Arc::new(|_| false),
        }
    }
}

pub struct MemoryMaintenance {
    options: MemoryMaintenanceOptions,
    scheduled: Arc<Mutex<HashSet<String>>>,
    swept: Arc<Mutex<HashSet<String>>>,
    tasks: Arc<Mutex<HashMap<String, tokio::task::JoinHandle<()>>>>,
    cancellation: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    wake: Arc<Mutex<HashMap<String, tokio::sync::watch::Sender<bool>>>>,
}

impl MemoryMaintenance {
    pub fn new(options: MemoryMaintenanceOptions) -> Self {
        Self {
            options,
            scheduled: Arc::new(Mutex::new(HashSet::new())),
            swept: Arc::new(Mutex::new(HashSet::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            cancellation: Arc::new(Mutex::new(HashMap::new())),
            wake: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn schedule(&self, context: MemoryIdentityContext) {
        if self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(&context.identity)
        {
            return;
        }
        if !self
            .scheduled
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(context.identity.clone())
        {
            return;
        }
        let should_sweep = self
            .swept
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(context.identity.clone());
        let options = self.options.clone();
        let scheduled = self.scheduled.clone();
        let tasks = self.tasks.clone();
        let cancellation = Arc::new(AtomicBool::new(false));
        self.cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(context.identity.clone(), cancellation.clone());
        let cancellation_map = self.cancellation.clone();
        let (wake_tx, mut wake_rx) = tokio::sync::watch::channel(false);
        self.wake
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(context.identity.clone(), wake_tx);
        let wake_map = self.wake.clone();
        let identity = context.identity.clone();
        let task_identity = identity.clone();
        let (registered, registration) = tokio::sync::oneshot::channel();
        let handle = tokio::spawn(async move {
            if registration.await.is_err() {
                return;
            }
            // The sweep inspects every retained transcript journal; it runs off the caller's
            // (interactive) thread, and `settled` still observes it because it precedes the delay.
            if should_sweep {
                let (sweep_context, sweep_options) = (context.clone(), options.clone());
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    sweep_journals(&sweep_context, &sweep_options)
                })
                .await
                    && let Some(logger) = &options.logger
                {
                    logger.warn(&format!(
                        "omo-senpi memory empty transcript sweep failed: {error}"
                    ));
                }
            }
            tokio::select! { _ = tokio::time::sleep(options.delay) => {}, _ = wake_rx.changed() => {} }
            if cancellation.load(Ordering::Acquire) {
                scheduled
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&task_identity);
                tasks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&task_identity);
                cancellation_map
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&task_identity);
                wake_map
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&task_identity);
                return;
            }
            let result = tokio::task::spawn_blocking({
                let context = context.clone();
                let options = options.clone();
                let cancellation = cancellation.clone();
                move || maintain(&context, &options, &cancellation)
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result);
            if let Err(error) = result
                && !cancellation.load(Ordering::Acquire)
                && let Some(logger) = &options.logger
            {
                logger.warn(&format!(
                    "omo-senpi memory repo maintenance failed: {error}"
                ));
            }
            scheduled
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task_identity);
            tasks
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task_identity);
            cancellation_map
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task_identity);
            wake_map
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&task_identity);
        });
        self.tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(identity, handle);
        let _ = registered.send(());
    }

    pub fn dispose(&self) {
        for token in self
            .cancellation
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            token.store(true, Ordering::Release);
        }
        for sender in self.wake.lock().unwrap_or_else(|e| e.into_inner()).values() {
            let _ = sender.send(true);
        }
    }

    pub async fn settled(&self) {
        let tasks = self
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, task)| task)
            .collect::<Vec<_>>();
        for task in tasks {
            let _ = task.await;
        }
    }
}

fn maintain(
    context: &MemoryIdentityContext,
    options: &MemoryMaintenanceOptions,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    if !context.identity_paths.repo.exists() {
        return Ok(());
    }
    let lock_path = context.identity_paths.locks.join("memory-maintenance.lock");
    fs::create_dir_all(&context.identity_paths.locks).map_err(|e| e.to_string())?;
    let lock = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let result = maintain_locked(context, options, cancelled);
    drop(lock);
    let _ = fs::remove_file(lock_path);
    result
}

fn sweep_journals(context: &MemoryIdentityContext, options: &MemoryMaintenanceOptions) {
    match memory_core::journal::sweep_empty_transcript_journals(
        &context.identity_paths.transcripts,
        (options.now)(),
        10 * 60 * 1000,
        options.is_live_session.as_ref(),
    ) {
        Ok(result) if !result.removed.is_empty() => {
            if let Some(logger) = &options.logger {
                logger.info("omo-senpi memory removed empty transcript journals");
            }
        }
        Ok(_) => {}
        Err(error) => {
            if let Some(logger) = &options.logger {
                logger.warn(&format!(
                    "omo-senpi memory empty transcript sweep failed: {error}"
                ));
            }
        }
    }
}

fn maintain_locked(
    context: &MemoryIdentityContext,
    options: &MemoryMaintenanceOptions,
    cancelled: &AtomicBool,
) -> Result<(), String> {
    // Finish applied-facts envelopes an interrupted onboarding apply left behind (pin `facts-applied-reconcile.ts`).
    if !cancelled.load(Ordering::Acquire) {
        let identity = memory_core::identity::resolve::MemoryIdentity {
            id: context.identity.clone(),
            safe_slug: memory_core::identity::resolve::sanitize_to_slug(&context.identity),
            paths: context.identity_paths.clone(),
        };
        let lock = crate::facts_apply_rpc::facts_writer_lock(
            identity.paths.locks.clone(),
            "facts-applied-reconcile".into(),
        );
        crate::facts_applied_reconcile::reconcile_applied_facts_batches(
            &identity,
            &lock,
            &mut |message, batch_id, outcome| {
                if let Some(logger) = &options.logger {
                    logger.warn(&format!(
                        "omo-senpi {message}: batchId={batch_id} outcome={outcome}"
                    ));
                }
            },
        );
    }
    let now = (options.now)();
    match memory_core::git::maintain_repository(
        &context.identity_paths.repo,
        "omo.maintenanceAt",
        now,
        options.interval.as_millis() as u64,
        &memory_core::git::GitMaintenanceOptions {
            min_loose_objects: options.min_loose_objects,
            timeout_ms: options.timeout.as_millis() as u64,
            executable: options.executable.clone(),
        },
        cancelled,
    )
    .map_err(|e| e.to_string())?
    {
        memory_core::git::GitMaintenanceResult::Skipped { .. }
        | memory_core::git::GitMaintenanceResult::IntervalSuppressed => {}
        memory_core::git::GitMaintenanceResult::Packed { .. } => {
            if let Some(logger) = &options.logger {
                logger.info("omo-senpi memory repo packed");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::binding::create_memory_binding;
    use memory_core::identity::layout::build_identity_paths;
    use std::path::Path;
    use std::process::Command;

    #[derive(Clone, Default)]
    struct Logger {
        info: Arc<Mutex<Vec<String>>>,
        warn: Arc<Mutex<Vec<String>>>,
    }
    impl MemoryMaintenanceLogger for Logger {
        fn info(&self, message: &str) {
            self.info.lock().unwrap().push(message.into());
        }
        fn warn(&self, message: &str) {
            self.warn.lock().unwrap().push(message.into());
        }
    }

    fn context(root: &Path) -> MemoryIdentityContext {
        let paths = build_identity_paths(root, "agent");
        MemoryIdentityContext::new(
            "agent".into(),
            paths.clone(),
            create_memory_binding("agent", paths.repo.to_string_lossy().as_ref(), 1.0),
        )
    }

    fn git(repo: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(repo)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn history(commits: usize) -> (tempfile::TempDir, MemoryIdentityContext) {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        fs::create_dir_all(&context.identity_paths.repo).unwrap();
        git(&context.identity_paths.repo, &["init", "-q"]);
        git(
            &context.identity_paths.repo,
            &["config", "user.email", "test@example.com"],
        );
        git(
            &context.identity_paths.repo,
            &["config", "user.name", "test"],
        );
        for index in 0..commits {
            fs::write(
                context.identity_paths.repo.join(format!("note-{index}.md")),
                format!("note {index}\n"),
            )
            .unwrap();
            git(&context.identity_paths.repo, &["add", "."]);
            git(
                &context.identity_paths.repo,
                &["commit", "-q", "-m", &format!("note {index}")],
            );
        }
        (root, context)
    }

    fn loose(repo: &Path) -> usize {
        git(repo, &["count-objects", "-v"])
            .lines()
            .find_map(|line| line.strip_prefix("count: ").and_then(|v| v.parse().ok()))
            .unwrap()
    }

    fn logger() -> Logger {
        Logger::default()
    }

    #[tokio::test]
    async fn packs_loose_objects_and_keeps_commits_readable() {
        let (_root, context) = history(20);
        let log = logger();
        let before = loose(&context.identity_paths.repo);
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions {
            logger: Some(Arc::new(log.clone())),
            delay: Duration::ZERO,
            min_loose_objects: 10,
            ..Default::default()
        });
        maintenance.schedule(context.clone());
        maintenance.settled().await;
        assert!(before >= 60);
        assert!(loose(&context.identity_paths.repo) < before);
        assert_eq!(
            *log.info.lock().unwrap(),
            vec!["omo-senpi memory repo packed"]
        );
        assert!(log.warn.lock().unwrap().is_empty());
        assert_eq!(
            git(
                &context.identity_paths.repo,
                &["rev-list", "--count", "HEAD"]
            )
            .trim(),
            "20"
        );
    }

    #[tokio::test]
    async fn concurrent_commit_survives_pack() {
        let (_root, context) = history(20);
        let repo = context.identity_paths.repo.clone();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::from_millis(50),
            min_loose_objects: 10,
            ..Default::default()
        });
        maintenance.schedule(context.clone());
        fs::write(repo.join("concurrent.md"), "written during maintenance\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "concurrent note"]);
        maintenance.settled().await;
        assert!(
            git(&repo, &["log", "--format=%s"])
                .lines()
                .any(|line| line == "concurrent note")
        );
        assert!(!git(&repo, &["fsck", "--connectivity-only"]).contains("missing"));
    }

    #[tokio::test]
    async fn recent_pass_skips_second_process() {
        let (_root, context) = history(20);
        let first = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::ZERO,
            min_loose_objects: 10,
            ..Default::default()
        });
        first.schedule(context.clone());
        first.settled().await;
        for index in 20..40 {
            fs::write(
                context.identity_paths.repo.join(format!("note-{index}.md")),
                format!("note {index}\n"),
            )
            .unwrap();
            git(&context.identity_paths.repo, &["add", "."]);
            git(
                &context.identity_paths.repo,
                &["commit", "-q", "-m", &format!("note {index}")],
            );
        }
        let before = loose(&context.identity_paths.repo);
        let log = logger();
        let second = MemoryMaintenance::new(MemoryMaintenanceOptions {
            logger: Some(Arc::new(log.clone())),
            delay: Duration::ZERO,
            min_loose_objects: 10,
            ..Default::default()
        });
        second.schedule(context.clone());
        second.settled().await;
        assert_eq!(loose(&context.identity_paths.repo), before);
        assert!(log.info.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_repository_is_a_noop() {
        let root = tempfile::tempdir().unwrap();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::ZERO,
            now: Arc::new(|| 1_000),
            ..Default::default()
        });
        maintenance.schedule(context(root.path()));
        maintenance.settled().await;
    }

    #[tokio::test]
    async fn ten_runners_pack_once() {
        let (_root, context) = history(20);
        let log = logger();
        let runners: Vec<_> = (0..10)
            .map(|_| {
                MemoryMaintenance::new(MemoryMaintenanceOptions {
                    logger: Some(Arc::new(log.clone())),
                    delay: Duration::ZERO,
                    min_loose_objects: 10,
                    ..Default::default()
                })
            })
            .collect();
        for runner in &runners {
            runner.schedule(context.clone());
        }
        for runner in &runners {
            runner.settled().await;
        }
        assert!(
            log.warn.lock().unwrap().is_empty(),
            "maintenance warnings: {:?}",
            *log.warn.lock().unwrap()
        );
        assert_eq!(
            *log.info.lock().unwrap(),
            vec!["omo-senpi memory repo packed"]
        );
        assert_eq!(loose(&context.identity_paths.repo), 0);
    }

    #[tokio::test]
    async fn held_lock_skips_without_warning() {
        let (_root, context) = history(20);
        fs::create_dir_all(&context.identity_paths.locks).unwrap();
        fs::write(
            context.identity_paths.locks.join("memory-maintenance.lock"),
            "held",
        )
        .unwrap();
        let before = loose(&context.identity_paths.repo);
        let log = logger();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions {
            logger: Some(Arc::new(log.clone())),
            delay: Duration::ZERO,
            min_loose_objects: 10,
            ..Default::default()
        });
        maintenance.schedule(context.clone());
        maintenance.settled().await;
        assert_eq!(loose(&context.identity_paths.repo), before);
        assert!(log.info.lock().unwrap().is_empty());
        assert!(log.warn.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn pending_loose_object_is_kept() {
        let (_root, context) = history(20);
        let pending_path = context.identity_paths.repo.join("pending.txt");
        fs::write(&pending_path, "staged by a writer\n").unwrap();
        let pending = git(
            &context.identity_paths.repo,
            &["hash-object", "-w", "pending.txt"],
        )
        .trim()
        .to_owned();
        fs::remove_file(pending_path).unwrap();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::ZERO,
            min_loose_objects: 10,
            ..Default::default()
        });
        maintenance.schedule(context.clone());
        maintenance.settled().await;
        assert_eq!(
            git(&context.identity_paths.repo, &["cat-file", "-p", &pending]),
            "staged by a writer\n"
        );
    }

    #[tokio::test]
    async fn disposed_host_can_schedule_again() {
        let (_root, context) = history(20);
        let log = logger();
        let host = MemoryMaintenance::new(MemoryMaintenanceOptions {
            logger: Some(Arc::new(log.clone())),
            delay: Duration::ZERO,
            min_loose_objects: 10,
            ..Default::default()
        });
        host.schedule(context.clone());
        host.dispose();
        host.settled().await;
        host.schedule(context.clone());
        host.settled().await;
        assert_eq!(
            *log.info.lock().unwrap(),
            vec!["omo-senpi memory repo packed"]
        );
        assert_eq!(loose(&context.identity_paths.repo), 0);
    }

    #[tokio::test]
    async fn dispose_cancels_delayed_pass() {
        let root = tempfile::tempdir().unwrap();
        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::from_secs(60),
            ..Default::default()
        });
        maintenance.schedule(context(root.path()));
        maintenance.dispose();
        maintenance.settled().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dispose_kills_and_reaps_running_git_child() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        fs::create_dir_all(&context.identity_paths.repo).unwrap();
        let executable = root.path().join("fake-git");
        let started_fifo = root.path().join("started.fifo");
        let release_fifo = root.path().join("release.fifo");
        let child_pid_path = root.path().join("child-pid");
        assert!(
            Command::new("mkfifo")
                .arg(&started_fifo)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("mkfifo")
                .arg(&release_fifo)
                .status()
                .unwrap()
                .success()
        );
        fs::write(
            &executable,
            format!("#!/bin/sh\nprintf '%s\\n' \"$$\" > '{}'\nprintf 'started\\n' > '{}'\nIFS= read -r line < '{}'\n", child_pid_path.display(), started_fifo.display(), release_fifo.display()),
        )
        .unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();

        let maintenance = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::ZERO,
            interval: Duration::ZERO,
            min_loose_objects: 0,
            timeout: Duration::from_secs(30),
            now: Arc::new(|| 1_000),
            executable: Some(executable),
            ..Default::default()
        });

        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let fifo = started_fifo.clone();
        let reader = std::thread::spawn(move || {
            let file = fs::File::open(fifo).unwrap();
            let mut line = String::new();
            std::io::BufRead::read_line(&mut std::io::BufReader::new(file), &mut line).unwrap();
            started_tx.send(line).unwrap();
        });
        maintenance.schedule(context);
        let started =
            tokio::task::spawn_blocking(move || started_rx.recv_timeout(Duration::from_secs(10)))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(started, "started\n");
        reader.join().unwrap();
        let child_pid = fs::read_to_string(child_pid_path)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        maintenance.dispose();
        tokio::time::timeout(Duration::from_secs(10), maintenance.settled())
            .await
            .unwrap();
        assert!(!std::path::Path::new(&format!("/proc/{child_pid}")).exists());
    }

    #[tokio::test]
    async fn journal_sweep_keeps_live_session_and_removes_idle_empty_session() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        for id in ["stale", "live"] {
            let dir = context.identity_paths.transcripts.join(id);
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("transcript.jsonl"), "").unwrap();
            fs::write(
                dir.join("state.json"),
                r#"{"schema_version":"v3_assistant_steps","total_completed_steps":0}"#,
            )
            .unwrap();
        }
        let log = logger();
        let host = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::ZERO,
            now: Arc::new(|| u64::MAX),
            is_live_session: Arc::new(|id| id == "live"),
            logger: Some(Arc::new(log.clone())),
            ..Default::default()
        });
        host.schedule(context.clone());
        host.settled().await;
        assert!(!context.identity_paths.transcripts.join("stale").exists());
        assert!(context.identity_paths.transcripts.join("live").exists());
        assert_eq!(
            *log.info.lock().unwrap(),
            vec!["omo-senpi memory removed empty transcript journals"]
        );
        assert!(log.warn.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn immediate_dispose_preserves_completed_journal_sweep() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        let dir = context.identity_paths.transcripts.join("health-check");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("transcript.jsonl"), "").unwrap();
        let host = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::from_secs(60),
            now: Arc::new(|| u64::MAX),
            ..Default::default()
        });
        host.schedule(context);
        host.dispose();
        host.settled().await;
        assert!(!dir.exists());
    }

    #[tokio::test]
    async fn journal_sweep_runs_only_once_per_identity_after_dispose() {
        let root = tempfile::tempdir().unwrap();
        let context = context(root.path());
        let host = MemoryMaintenance::new(MemoryMaintenanceOptions {
            delay: Duration::from_secs(60),
            now: Arc::new(|| u64::MAX),
            ..Default::default()
        });
        host.schedule(context.clone());
        host.dispose();
        host.settled().await;
        let dir = context.identity_paths.transcripts.join("later-empty");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("transcript.jsonl"), "").unwrap();
        host.schedule(context);
        host.dispose();
        host.settled().await;
        assert!(dir.exists());
    }
}
