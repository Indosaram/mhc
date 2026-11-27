//! Port of `packages/coding-agent/src/core/kill-guard/index.ts` and submodules (senpi#5349).

pub mod check_command;
pub mod check_entry;
pub mod command;
pub mod ere;
pub mod host;
pub mod kill;
pub mod options;
pub mod pattern;
pub mod process_view;
pub mod protected;
pub mod request;
pub mod runtime;
pub mod selecting;
pub mod selection;
pub mod shim_script;
pub mod situation;
pub mod types;
pub mod verdict;

pub use check_command::{
    KILL_GUARD_COMMAND, KillGuardCheckOptions, run_kill_guard_check, run_kill_guard_check_command,
};
pub use command::{judge_argv, judge_command, judge_signal};
pub use host::{
    KillGuardSetting, host_kill_guard_context, kill_guard_protected_pids, read_kill_guard_setting,
};
pub use pattern::{PatternMode, PatternVerdict, judge_pattern};
pub use process_view::{
    KillGuardContextOptions, KillGuardSpawnSync, SpawnOutput, create_process_view,
    kill_guard_context, system_spawn_sync,
};
pub use request::{
    KillGuardRequest, KillGuardRequestOptions, decide_kill_guard_request, parse_kill_guard_request,
};
pub use runtime::{
    ActivateKillGuardRuntimeOptions, KillGuardHolder, KillGuardRuntime,
    activate_kill_guard_runtime, active_kill_guard_environment, active_kill_guard_runtime,
    install_kill_guard_shims, kill_guard_environment, release_kill_guard_runtime,
    with_kill_guard_runtime,
};
pub use shim_script::{
    KILL_GUARD_BASH_ENV, KILL_GUARD_BASH_KILL, KILL_GUARD_JUDGE_DEADLINE_SECONDS,
    KILL_GUARD_PROMPT_HOOK_PREFIX, KILL_GUARD_SHIM_MARKER, KILL_GUARD_SHIM_NAMES,
    KillGuardShimFiles, kill_guard_prompt_command, render_kill_guard_shims,
};
pub use types::{
    KillGuardContext, KillGuardRule, KillGuardShell, KillGuardVantage, KillGuardVerdict,
    ProcessRow, ProcessView, SelectResult, SignalCall, SignalId,
};
pub use verdict::KILL_GUARD_TAG;
