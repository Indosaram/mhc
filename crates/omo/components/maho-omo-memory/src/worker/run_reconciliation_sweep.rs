//! Reclaims reflection leftovers a reconciliation pass found (pin `run-reconciliation-sweep.ts`).
//!
//! Reconciliation only repairs runs that still own a run directory. Worktrees and branches from
//! runs whose directory was already removed, or that died between `git worktree add` and the
//! ledger write, survive every finalization path, so a reconciliation pass also sweeps them.
//! Ownership is proven, never guessed: the reservation's active/pending runs and every
//! non-terminal run directory are live, and anything younger than the grace window is left alone.

use std::collections::BTreeSet;
use std::path::Path;

use memory_core::git::GitMemoryRepo;
use memory_core::identity::resolve::MemoryIdentity;
use memory_core::reflection::{
    OrphanKind, REFLECTION_ORPHAN_GRACE_MS, ReflectionOrphanReceipt, ReflectionOrphanSweepOptions,
    sweep_reflection_orphans,
};

use super::runner_types::ReflectionReservationPort;

/// Port of the upstream `ReflectionSweepLogger` warning sink.
pub trait ReflectionSweepLogger {
    fn warn(&self, message: &str, details: Option<&ReflectionSweepDetails>);
}

/// The upstream `{ kind, target, detail }` warning payload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReflectionSweepDetails {
    pub kind: Option<OrphanKind>,
    pub target: Option<String>,
    pub detail: Option<String>,
}

/// Port of the upstream `ReflectionOrphanSweepContext`.
pub struct ReflectionOrphanSweepContext<'a> {
    pub identity: &'a MemoryIdentity,
    pub reservation: &'a dyn ReflectionReservationPort,
    pub now_ms: f64,
    pub logger: Option<&'a dyn ReflectionSweepLogger>,
    pub defer_on_scheduler_contention: bool,
}

/// Port of `sweepReflectionRunOrphans`: leftovers are maintenance, never a reason to fail the pass
/// that repairs live runs. Only the live-run probe propagates, exactly as upstream rethrows the
/// `collectLiveRunIds` failure.
pub fn sweep_reflection_run_orphans(
    context: &ReflectionOrphanSweepContext<'_>,
) -> Result<(), String> {
    let state = match context.reservation.read_state_with_wait(if context.defer_on_scheduler_contention { Some(0) } else { None }) {
        Ok(state) => state,
        Err(memory_core::reflection::ReservationError::Contention(_)) if context.defer_on_scheduler_contention => return Ok(()),
        Err(error) => return Err(error.to_string()),
    };
    let live_run_ids = collect_live_run_ids(context, &state)?;
    let repo = match GitMemoryRepo::open(&context.identity.paths.repo, &context.identity.id) {
        Ok(repo) => repo,
        Err(error) => {
            warn_sweep_failure(context, error.to_string());
            return Ok(());
        }
    };
    match sweep_reflection_orphans(
        &repo,
        &context.identity.paths.worktrees,
        &ReflectionOrphanSweepOptions {
            live_run_ids,
            now_ms: context.now_ms,
            grace_ms: Some(REFLECTION_ORPHAN_GRACE_MS),
            exec: None,
        },
    ) {
        Ok(receipts) => {
            for receipt in &receipts {
                warn_receipt(context, receipt);
            }
        }
        Err(error) => warn_sweep_failure(context, error),
    }
    Ok(())
}

fn collect_live_run_ids(
    context: &ReflectionOrphanSweepContext<'_>,
    state: &memory_core::reflection::ReservationState,
) -> Result<BTreeSet<String>, String> {
    let mut live = BTreeSet::new();
    if let Some(active) = &state.active {
        live.insert(active.run_id.clone());
    }
    if let Some(pending) = &state.pending {
        live.insert(pending.run_id.clone());
    }
    let runs_dir = context.identity.paths.reflection.join("runs");
    for run_id in run_directory_names(&runs_dir)? {
        let run_dir = runs_dir.join(&run_id);
        if run_dir.join("final.json").exists() || run_dir.join("abandoned.json").exists() {
            continue;
        }
        // prelaunch.json is written before the worktree exists, so it claims the directory early.
        if !run_dir.join("ledger.json").exists() && !run_dir.join("prelaunch.json").exists() {
            continue;
        }
        live.insert(run_id);
    }
    Ok(live)
}

fn run_directory_names(path: &Path) -> Result<Vec<String>, String> {
    let entries = match std::fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.to_string()),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        if entry
            .file_type()
            .map_err(|error| error.to_string())?
            .is_dir()
        {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(names)
}

fn warn_receipt(context: &ReflectionOrphanSweepContext<'_>, receipt: &ReflectionOrphanReceipt) {
    let Some(logger) = context.logger else {
        return;
    };
    let message = if receipt.removed {
        "Discarded an orphaned reflection leftover"
    } else {
        "Could not discard an orphaned reflection leftover"
    };
    logger.warn(
        message,
        Some(&ReflectionSweepDetails {
            kind: Some(receipt.kind),
            target: Some(receipt.target.clone()),
            detail: receipt.detail.clone(),
        }),
    );
}

fn warn_sweep_failure(context: &ReflectionOrphanSweepContext<'_>, detail: String) {
    let Some(logger) = context.logger else {
        return;
    };
    logger.warn(
        "Reflection orphan sweep failed",
        Some(&ReflectionSweepDetails {
            detail: Some(detail),
            ..Default::default()
        }),
    );
}

pub struct ReflectionSweepWarnLogger<'a>(pub &'a (dyn Fn(&str) + Send + Sync));

impl ReflectionSweepLogger for ReflectionSweepWarnLogger<'_> {
    fn warn(&self, message: &str, details: Option<&ReflectionSweepDetails>) {
        match details {
            Some(details) => (self.0)(&format!("{message}: {details:?}")),
            None => (self.0)(message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;

    use memory_core::git::InitializeGitRepoOptions;
    use memory_core::identity::layout::build_identity_paths;
    use memory_core::reflection::{
        CompletionResult, ReflectionOutcome, ReservationState, create_reflection_worktree,
        discard_reflection_worktree,
    };

    const HOUR_MS: f64 = 60.0 * 60_000.0;

    struct Reservation(ReservationState);
    impl ReflectionReservationPort for Reservation {
        fn read_state(&self) -> Result<ReservationState, String> {
            Ok(self.0.clone())
        }
        fn complete(&self, _: &str, _: ReflectionOutcome) -> Result<CompletionResult, String> {
            panic!("the orphan sweep never completes a reservation")
        }
    }

    #[derive(Default)]
    struct Recording {
        warnings: Mutex<Vec<(String, Option<ReflectionSweepDetails>)>>,
    }
    impl ReflectionSweepLogger for Recording {
        fn warn(&self, message: &str, details: Option<&ReflectionSweepDetails>) {
            self.warnings
                .lock()
                .unwrap()
                .push((message.to_owned(), details.cloned()));
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        identity: MemoryIdentity,
        repo: GitMemoryRepo,
        runs: PathBuf,
        worktrees: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let identity = MemoryIdentity {
            id: "sweep-agent".to_string(),
            safe_slug: "sweep-agent".to_string(),
            paths: build_identity_paths(dir.path(), "sweep-agent"),
        };
        std::fs::create_dir_all(&identity.paths.worktrees).expect("worktrees dir");
        std::fs::create_dir_all(identity.paths.reflection.join("runs")).expect("runs dir");
        let repo = GitMemoryRepo::open(&identity.paths.repo, "sweep-agent").expect("repo");
        repo.init(Some(InitializeGitRepoOptions::default()))
            .expect("init");
        Fixture {
            runs: identity.paths.reflection.join("runs"),
            worktrees: identity.paths.worktrees.clone(),
            _dir: dir,
            identity,
            repo,
        }
    }

    /// An aged worktree left by a run whose directory is long gone.
    fn orphan_worktree(
        fixture: &Fixture,
        run_id: &str,
    ) -> memory_core::reflection::ReflectionWorktree {
        let exec = fixture.repo.exec();
        create_reflection_worktree(&fixture.repo, run_id, &fixture.worktrees, exec.as_ref(), None)
            .expect("orphan worktree")
    }

    fn claim_run(fixture: &Fixture, run_id: &str) {
        let dir = fixture.runs.join(run_id);
        std::fs::create_dir_all(&dir).expect("run dir");
        std::fs::write(dir.join("ledger.json"), "{}").expect("ledger");
    }

    fn reserved(run_id: &str) -> ReservationState {
        ReservationState {
            active: Some(memory_core::reflection::ReservedRun {
                run_id: run_id.to_string(),
                request: memory_core::reflection::ReflectionRequest {
                    trigger: memory_core::reflection::ReflectionTrigger::Manual,
                    origin: None,
                    conversation_ids: Vec::new(),
                    snapshots: Vec::new(),
                    focus: None,
                    recent_n: None,
                    target_doc: None,
                },
                reserved_at: None,
                launcher_pid: None,
                launcher_hostname: None,
                launcher_process_start: None,
            }),
            pending: None,
        }
    }

    #[test]
    fn an_aged_orphan_is_reclaimed_while_a_live_run_survives() {
        let fixture = fixture();
        claim_run(&fixture, "run-live");
        let live = orphan_worktree(&fixture, "run-live");
        let orphan = orphan_worktree(&fixture, "run-gone");
        let logger = Recording::default();
        let now = memory_core::support::time::now_millis() as f64 + HOUR_MS;

        let result = sweep_reflection_run_orphans(&ReflectionOrphanSweepContext {
            identity: &fixture.identity,
            reservation: &Reservation(ReservationState::default()),
            now_ms: now,
            logger: Some(&logger),
            defer_on_scheduler_contention: false,
        });

        assert_eq!(result, Ok(()));
        assert!(!orphan.dir.exists());
        assert!(live.dir.exists());
        let warnings = logger.warnings.lock().unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, "Discarded an orphaned reflection leftover");
        let details = warnings[0].1.as_ref().expect("receipt details");
        assert_eq!(details.kind, Some(OrphanKind::Worktree));
        assert_eq!(
            details.target.as_deref(),
            Some(orphan.dir.to_string_lossy().as_ref())
        );
        drop(warnings);

        let exec = fixture.repo.exec();
        let cleanup = discard_reflection_worktree(&fixture.repo, &live.dir, &live.branch, exec.as_ref());
        assert!(cleanup.worktree_removed && cleanup.branch_removed);
    }

    #[test]
    fn a_reserved_run_id_protects_its_worktree() {
        let fixture = fixture();
        let protected = orphan_worktree(&fixture, "run-reserved");

        let result = sweep_reflection_run_orphans(&ReflectionOrphanSweepContext {
            identity: &fixture.identity,
            reservation: &Reservation(reserved("run-reserved")),
            now_ms: memory_core::support::time::now_millis() as f64 + HOUR_MS,
            logger: None,
            defer_on_scheduler_contention: false,
        });

        assert_eq!(result, Ok(()));
        assert!(protected.dir.exists());
        let exec = fixture.repo.exec();
        let cleanup = discard_reflection_worktree(&fixture.repo, &protected.dir, &protected.branch, exec.as_ref());
        assert!(cleanup.worktree_removed && cleanup.branch_removed);
    }

    #[test]
    fn an_orphan_inside_the_grace_window_is_left_alone() {
        let fixture = fixture();
        let young = orphan_worktree(&fixture, "run-young");

        let result = sweep_reflection_run_orphans(&ReflectionOrphanSweepContext {
            identity: &fixture.identity,
            reservation: &Reservation(ReservationState::default()),
            now_ms: memory_core::support::time::now_millis() as f64,
            logger: None,
            defer_on_scheduler_contention: false,
        });

        assert_eq!(result, Ok(()));
        assert!(young.dir.exists());
        let exec = fixture.repo.exec();
        let cleanup = discard_reflection_worktree(&fixture.repo, &young.dir, &young.branch, exec.as_ref());
        assert!(cleanup.worktree_removed && cleanup.branch_removed);
    }

    #[test]
    fn a_failing_sweep_reports_a_warning_without_failing_the_pass() {
        let fixture = fixture();
        let logger = Recording::default();

        // A run directory without a repository to sweep: the git listing cannot succeed.
        let missing = MemoryIdentity {
            id: fixture.identity.id.clone(),
            safe_slug: fixture.identity.safe_slug.clone(),
            paths: memory_core::identity::layout::build_identity_paths(
                &fixture.worktrees.join("absent"),
                "sweep-agent",
            ),
        };
        let result = sweep_reflection_run_orphans(&ReflectionOrphanSweepContext {
            identity: &missing,
            reservation: &Reservation(ReservationState::default()),
            now_ms: 0.0,
            logger: Some(&logger),
            defer_on_scheduler_contention: false,
        });

        assert_eq!(result, Ok(()));
        let warnings = logger.warnings.lock().unwrap();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].0, "Reflection orphan sweep failed");
        assert!(
            warnings[0]
                .1
                .as_ref()
                .is_some_and(|details| details.detail.is_some())
        );
    }
}
