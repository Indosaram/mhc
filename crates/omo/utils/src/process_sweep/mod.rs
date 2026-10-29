//! Process-sweep families: token-aware classifiers over the process table
//! (codegraph workers/daemons, lsp-daemon proxies, stale lsp-daemon versions)
//! plus a throttled terminate-then-kill executor.

mod codegraph_family;
mod codegraph_sweeper;
mod command_match;
mod exec;
mod family_sweeper;
mod lsp_daemon_family;
mod lsp_daemon_owner_attestation;
mod lsp_proxy_family;
mod process_table;
mod roots;
mod sweeper;

pub use codegraph_family::{
    CodegraphProcessMatchKind, CodegraphZombieProcess, SelectZombieCodegraphProcessesOptions,
    select_zombie_codegraph_processes,
};
pub use codegraph_sweeper::{
    CodegraphSweepAction, ProcessProvider, SweepCodegraphZombiesOptions,
    SweepCodegraphZombiesResult, sweep_codegraph_zombies,
};
pub use command_match::{
    find_token_end, find_token_start, has_executable_token,
    has_executable_token_under_root_with_suffix, normalize_for_comparison, normalize_roots,
    resolve_path_for_platform, split_command_tokens, token_looks_executable,
};
pub use exec::{
    CodegraphProcessKiller, ProcessKiller, create_default_codegraph_process_killer,
    create_default_process_killer, default_is_process_alive, enumerate_codegraph_processes,
    enumerate_processes,
};
pub use family_sweeper::{
    FamilySweepConfig, FamilySweepPlan, ProcessFamilySweepOptions, ProcessFamilySweepResult,
    ProcessSweepAction, ProcessSweepFailure, ProcessSweepStage, SignalAttestation, SweepTarget,
    run_process_family_sweep,
};
pub use lsp_daemon_family::{
    LspDaemonAttestTarget, LspDaemonAttestationDeps, LspDaemonBaseDirOptions,
    LspDaemonExecuteForStdout, LspDaemonReadProcFile, LspDaemonVersionDir, OMO_LSP_DAEMON_DIR_ENV,
    OMO_LSP_DAEMON_VERSION_ENV, PlanStaleLspDaemonVersionSweepOptions, SparedLspDaemonReason,
    SparedLspDaemonVersion, StaleLspDaemonVersionSweepPlan, StaleLspDaemonVersionTarget,
    attest_lsp_daemon_cli_process, list_lsp_daemon_version_dirs,
    plan_stale_lsp_daemon_version_sweep, read_lsp_daemon_owner_pid, read_lsp_daemon_owner_target,
    resolve_lsp_daemon_base_dir,
};
pub use lsp_daemon_owner_attestation::{
    LspDaemonOwnerAttestationDeps, LspDaemonOwnerEndpoint, LspDaemonOwnerIdentity,
    LspDaemonOwnerPing, LspDaemonOwnerTarget, LspDaemonReadText, attest_lsp_daemon_owner,
    create_lsp_daemon_owner_ping_request, parse_lsp_daemon_owner,
};
pub use lsp_proxy_family::{
    LspDaemonProxyMatchKind, LspDaemonProxyProcess, SelectOrphanedLspDaemonProxiesOptions,
    select_orphaned_lsp_daemon_proxies,
};
pub use process_table::{
    CodegraphProcessInfo, ProcessInfo, is_orphaned, parse_posix_process_table,
    parse_windows_process_table,
};
pub use roots::{
    CodegraphOwnedRootsOptions, discover_codegraph_owned_roots, discover_omo_owned_roots,
};
pub use sweeper::{
    LspDaemonPidAttest, LspDaemonVersionSweepAction, SweepOrphanedLspDaemonProxiesOptions,
    SweepOrphanedLspDaemonProxiesResult, SweepStaleLspDaemonVersionsOptions,
    SweepStaleLspDaemonVersionsResult, sweep_orphaned_lsp_daemon_proxies,
    sweep_stale_lsp_daemon_versions,
};
