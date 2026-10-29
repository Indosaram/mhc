use std::cell::OnceCell;
use std::path::PathBuf;

use crate::codegraph::{codegraph_data_root, evaluate_daemon_staleness};

use super::codegraph_family::{
    CodegraphProcessMatchKind, CodegraphZombieProcess, SelectZombieCodegraphProcessesOptions,
    select_zombie_codegraph_processes,
};
use super::exec::enumerate_processes;
use super::family_sweeper::{
    FamilySweepConfig, FamilySweepPlan, ProcessFamilySweepOptions, ProcessFamilySweepResult,
    ProcessSweepAction, ProcessSweepFailure, run_process_family_sweep,
};
use super::lsp_daemon_family::resolve_home_dir;
use super::process_table::ProcessInfo;
use super::roots::{CodegraphOwnedRootsOptions, discover_codegraph_owned_roots};

pub type CodegraphSweepAction = ProcessSweepAction;
pub type ProcessProvider<'a> = &'a dyn Fn() -> Result<Vec<ProcessInfo>, String>;

#[derive(Default)]
pub struct SweepCodegraphZombiesOptions<'a> {
    pub roots: CodegraphOwnedRootsOptions,
    pub sweep: ProcessFamilySweepOptions<'a>,
    pub owned_roots: Option<Vec<String>>,
    pub process_provider: Option<ProcessProvider<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepCodegraphZombiesResult {
    pub action: ProcessSweepAction,
    pub candidates: Vec<CodegraphZombieProcess>,
    pub daemon_action: ProcessSweepAction,
    pub daemon_stamp_file: PathBuf,
    pub dry_run: bool,
    pub failed: Vec<ProcessSweepFailure>,
    pub killed: Vec<CodegraphZombieProcess>,
    pub owned_roots: Vec<String>,
    pub spared: Vec<CodegraphZombieProcess>,
    pub stamp_file: PathBuf,
    pub worker_action: ProcessSweepAction,
    pub worker_stamp_file: PathBuf,
}

// Keep the legacy daemon stamp name so existing installs retain their cadence;
// worker selection is cheap and bypasses its stamp on every invocation.
const CODEGRAPH_DAEMON_SWEEP_STAMP_FILE: &str = "zombie-sweep.stamp";
const CODEGRAPH_WORKER_SWEEP_STAMP_FILE: &str = "worker-sweep.stamp";

pub fn sweep_codegraph_zombies(
    options: &SweepCodegraphZombiesOptions<'_>,
) -> SweepCodegraphZombiesResult {
    let roots_env = |key: &str| match &options.roots.env {
        Some(env) => env.get(key).cloned(),
        None => std::env::var(key).ok(),
    };
    let data_root = codegraph_data_root(&resolve_home_dir(
        options.roots.home_dir.as_deref(),
        roots_env,
    ));
    let daemon_stamp_file = data_root.join(CODEGRAPH_DAEMON_SWEEP_STAMP_FILE);
    let worker_stamp_file = data_root.join(CODEGRAPH_WORKER_SWEEP_STAMP_FILE);
    let owned_roots = options
        .owned_roots
        .clone()
        .unwrap_or_else(|| discover_codegraph_owned_roots(&options.roots));
    let platform = options.sweep.platform();

    let cached: OnceCell<Result<Vec<CodegraphZombieProcess>, String>> = OnceCell::new();
    let collect_candidates = || {
        cached
            .get_or_init(|| {
                let processes = match options.process_provider {
                    Some(provider) => provider(),
                    None => enumerate_processes(platform),
                }?;
                Ok(select_zombie_codegraph_processes(
                    &processes,
                    &SelectZombieCodegraphProcessesOptions {
                        owned_roots: &owned_roots,
                        platform: Some(platform),
                    },
                ))
            })
            .clone()
    };

    let collect_workers = || {
        let candidates: Vec<_> = collect_candidates()?
            .into_iter()
            .filter(|candidate| candidate.match_kind != CodegraphProcessMatchKind::UpstreamDaemon)
            .collect();
        Ok(FamilySweepPlan {
            kill_list: candidates.clone(),
            candidates,
            spared: Vec::new(),
        })
    };
    let worker_options = ProcessFamilySweepOptions {
        force: true,
        ..clone_sweep_options(&options.sweep)
    };
    let worker_result = run_process_family_sweep(
        &FamilySweepConfig {
            attest_before_signal: None,
            family_label: "CodeGraph worker sweep",
            stamp_file: worker_stamp_file.clone(),
            collect: &collect_workers,
        },
        &worker_options,
    );

    let collect_daemons = || {
        let candidates: Vec<_> = collect_candidates()?
            .into_iter()
            .filter(|candidate| candidate.match_kind == CodegraphProcessMatchKind::UpstreamDaemon)
            .collect();
        let (kill_list, spared) = partition_by_daemon_staleness(&candidates, &options.sweep);
        Ok(FamilySweepPlan {
            candidates,
            kill_list,
            spared,
        })
    };
    let daemon_result = run_process_family_sweep(
        &FamilySweepConfig {
            attest_before_signal: None,
            family_label: "CodeGraph daemon sweep",
            stamp_file: daemon_stamp_file.clone(),
            collect: &collect_daemons,
        },
        &options.sweep,
    );

    merge_results(
        worker_result,
        daemon_result,
        owned_roots,
        options.sweep.dry_run,
    )
}

fn clone_sweep_options<'a>(
    options: &ProcessFamilySweepOptions<'a>,
) -> ProcessFamilySweepOptions<'a> {
    ProcessFamilySweepOptions {
        dry_run: options.dry_run,
        force: options.force,
        grace_ms: options.grace_ms,
        killer: options.killer,
        log: options.log,
        now_ms: options.now_ms,
        platform: options.platform,
        throttle_ms: options.throttle_ms,
    }
}

fn merge_results(
    worker: ProcessFamilySweepResult<CodegraphZombieProcess>,
    daemon: ProcessFamilySweepResult<CodegraphZombieProcess>,
    owned_roots: Vec<String>,
    dry_run: bool,
) -> SweepCodegraphZombiesResult {
    let action = match (worker.action, daemon.action) {
        (ProcessSweepAction::Failed, _) | (_, ProcessSweepAction::Failed) => {
            ProcessSweepAction::Failed
        }
        (ProcessSweepAction::Swept, _) | (_, ProcessSweepAction::Swept) => {
            ProcessSweepAction::Swept
        }
        (ProcessSweepAction::Throttled, ProcessSweepAction::Throttled) => {
            ProcessSweepAction::Throttled
        }
    };
    SweepCodegraphZombiesResult {
        action,
        candidates: [worker.candidates, daemon.candidates].concat(),
        daemon_action: daemon.action,
        daemon_stamp_file: daemon.stamp_file.clone(),
        dry_run,
        failed: [worker.failed, daemon.failed].concat(),
        killed: [worker.killed, daemon.killed].concat(),
        owned_roots,
        spared: daemon.spared,
        stamp_file: daemon.stamp_file,
        worker_action: worker.action,
        worker_stamp_file: worker.stamp_file,
    }
}

fn partition_by_daemon_staleness(
    candidates: &[CodegraphZombieProcess],
    options: &ProcessFamilySweepOptions<'_>,
) -> (Vec<CodegraphZombieProcess>, Vec<CodegraphZombieProcess>) {
    let mut kill_list = Vec::new();
    let mut spared = Vec::new();
    for candidate in candidates {
        let project_root = candidate
            .daemon_project_root
            .as_deref()
            .unwrap_or(&candidate.matched_root);
        let staleness =
            evaluate_daemon_staleness(candidate.pid, std::path::Path::new(project_root));
        let pid = candidate.pid;
        let reason = staleness.reason.as_str();
        if staleness.stale {
            options.log(&format!(
                "CodeGraph daemon sweep sweeping stale daemon pid {pid} ({reason})"
            ));
            kill_list.push(candidate.clone());
        } else {
            options.log(&format!(
                "CodeGraph daemon sweep spared live daemon pid {pid} ({reason})"
            ));
            spared.push(candidate.clone());
        }
    }
    (kill_list, spared)
}
