use std::path::PathBuf;

use super::codegraph_sweeper::ProcessProvider;
use super::exec::enumerate_processes;
use super::family_sweeper::{
    FamilySweepConfig, FamilySweepPlan, ProcessFamilySweepOptions, ProcessFamilySweepResult,
    ProcessSweepAction, ProcessSweepFailure, run_process_family_sweep,
};
use super::lsp_daemon_family::{
    LspDaemonAttestTarget, LspDaemonBaseDirOptions, OMO_LSP_DAEMON_VERSION_ENV,
    PlanStaleLspDaemonVersionSweepOptions, SparedLspDaemonVersion, StaleLspDaemonVersionTarget,
    default_attest_target, plan_stale_lsp_daemon_version_sweep, resolve_lsp_daemon_base_dir,
};
use super::lsp_proxy_family::{
    LspDaemonProxyProcess, SelectOrphanedLspDaemonProxiesOptions,
    select_orphaned_lsp_daemon_proxies,
};
use super::roots::{CodegraphOwnedRootsOptions, discover_codegraph_owned_roots};

#[derive(Default)]
pub struct SweepOrphanedLspDaemonProxiesOptions<'a> {
    pub base_dir: LspDaemonBaseDirOptions,
    pub roots: CodegraphOwnedRootsOptions,
    pub sweep: ProcessFamilySweepOptions<'a>,
    pub owned_roots: Option<Vec<String>>,
    pub process_provider: Option<ProcessProvider<'a>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOrphanedLspDaemonProxiesResult {
    pub result: ProcessFamilySweepResult<LspDaemonProxyProcess>,
    pub owned_roots: Vec<String>,
}

const LSP_PROXY_SWEEP_STAMP_FILE: &str = "lsp-proxy-sweep.stamp";
const LSP_DAEMON_SWEEP_STAMP_FILE: &str = "lsp-daemon-sweep.stamp";

pub fn sweep_orphaned_lsp_daemon_proxies(
    options: &SweepOrphanedLspDaemonProxiesOptions<'_>,
) -> SweepOrphanedLspDaemonProxiesResult {
    let stamp_file =
        resolve_lsp_daemon_base_dir(&options.base_dir).join(LSP_PROXY_SWEEP_STAMP_FILE);
    let owned_roots = options
        .owned_roots
        .clone()
        .unwrap_or_else(|| discover_codegraph_owned_roots(&options.roots));
    let platform = options.sweep.platform();
    let collect = || {
        let processes = match options.process_provider {
            Some(provider) => provider(),
            None => enumerate_processes(platform),
        }?;
        let candidates = select_orphaned_lsp_daemon_proxies(
            &processes,
            &SelectOrphanedLspDaemonProxiesOptions {
                owned_roots: &owned_roots,
                platform: Some(platform),
            },
        );
        Ok(FamilySweepPlan {
            kill_list: candidates.clone(),
            candidates,
            spared: Vec::new(),
        })
    };
    let result = run_process_family_sweep(
        &FamilySweepConfig {
            attest_before_signal: None,
            family_label: "lsp-daemon proxy sweep",
            stamp_file,
            collect: &collect,
        },
        &options.sweep,
    );
    SweepOrphanedLspDaemonProxiesResult {
        result,
        owned_roots,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LspDaemonVersionSweepAction {
    Failed,
    Skipped,
    Swept,
    Throttled,
}

impl From<ProcessSweepAction> for LspDaemonVersionSweepAction {
    fn from(action: ProcessSweepAction) -> Self {
        match action {
            ProcessSweepAction::Failed => Self::Failed,
            ProcessSweepAction::Swept => Self::Swept,
            ProcessSweepAction::Throttled => Self::Throttled,
        }
    }
}

pub type LspDaemonPidAttest<'a> = &'a dyn Fn(u32, &str) -> bool;

#[derive(Default)]
pub struct SweepStaleLspDaemonVersionsOptions<'a> {
    pub base_dir: LspDaemonBaseDirOptions,
    pub sweep: ProcessFamilySweepOptions<'a>,
    /// Pid-only attestation (legacy); ignored when `attest_target` is set.
    pub attest: Option<LspDaemonPidAttest<'a>>,
    pub attest_target: Option<LspDaemonAttestTarget<'a>>,
    pub current_version: Option<String>,
    pub is_alive: Option<&'a dyn Fn(u32) -> bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepStaleLspDaemonVersionsResult {
    pub action: LspDaemonVersionSweepAction,
    pub candidates: Vec<StaleLspDaemonVersionTarget>,
    pub current_version: Option<String>,
    pub dry_run: bool,
    pub failed: Vec<ProcessSweepFailure>,
    pub killed: Vec<StaleLspDaemonVersionTarget>,
    pub spared: Vec<SparedLspDaemonVersion>,
    pub stamp_file: PathBuf,
}

pub fn sweep_stale_lsp_daemon_versions(
    options: &SweepStaleLspDaemonVersionsOptions<'_>,
) -> SweepStaleLspDaemonVersionsResult {
    let base_dir = resolve_lsp_daemon_base_dir(&options.base_dir);
    let stamp_file = base_dir.join(LSP_DAEMON_SWEEP_STAMP_FILE);
    let dry_run = options.sweep.dry_run;
    let current_version = options
        .current_version
        .clone()
        .filter(|version| !version.trim().is_empty())
        .or_else(|| {
            options
                .base_dir
                .env_value(OMO_LSP_DAEMON_VERSION_ENV)
                .filter(|version| !version.trim().is_empty())
        });
    let Some(current_version) = current_version else {
        options
            .sweep
            .log("lsp-daemon stale-version sweep skipped: current lsp-daemon version is unknown");
        return SweepStaleLspDaemonVersionsResult {
            action: LspDaemonVersionSweepAction::Skipped,
            candidates: Vec::new(),
            current_version: None,
            dry_run,
            failed: Vec::new(),
            killed: Vec::new(),
            spared: Vec::new(),
            stamp_file,
        };
    };

    let platform = options.sweep.platform();
    let pid_attest = |target: &StaleLspDaemonVersionTarget, platform: &str| {
        options
            .attest
            .is_some_and(|attest| attest(target.target.pid, platform))
    };
    let attest_target: LspDaemonAttestTarget<'_> = match (options.attest_target, options.attest) {
        (Some(attest_target), _) => attest_target,
        (None, Some(_)) => &pid_attest,
        (None, None) => &default_attest_target,
    };
    let attest_before_signal =
        |target: &StaleLspDaemonVersionTarget| Ok(attest_target(target, platform));
    let collect = || {
        let plan = plan_stale_lsp_daemon_version_sweep(&PlanStaleLspDaemonVersionSweepOptions {
            attest_target: Some(attest_target),
            base_dir: &base_dir,
            current_version: &current_version,
            is_alive: options.is_alive,
            log: options.sweep.log,
            platform: Some(platform),
        });
        let candidates = plan
            .targets
            .iter()
            .cloned()
            .chain(plan.spared.iter().map(|spared| spared.target.clone()))
            .collect();
        Ok(FamilySweepPlan {
            candidates,
            kill_list: plan.targets,
            spared: plan.spared,
        })
    };
    let result = run_process_family_sweep(
        &FamilySweepConfig {
            attest_before_signal: Some(&attest_before_signal),
            family_label: "lsp-daemon stale-version sweep",
            stamp_file,
            collect: &collect,
        },
        &options.sweep,
    );
    SweepStaleLspDaemonVersionsResult {
        action: result.action.into(),
        candidates: result.candidates,
        current_version: Some(current_version),
        dry_run: result.dry_run,
        failed: result.failed,
        killed: result.killed,
        spared: result.spared,
        stamp_file: result.stamp_file,
    }
}
