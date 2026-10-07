//! Process spawning, PATH lookup, file shims and Git Bash resolution.

mod file;
mod git_bash;
mod platform;
mod spawn;
mod which;

pub use file::{RuntimeFile, bun_file, bun_write};
pub use git_bash::{
    GIT_BASH_ENV_KEY, GitBashResolution, GitBashResolverInput, GitBashSource, WINGET_INSTALL_ARGS,
    resolve_git_bash, resolve_git_bash_for_current_process,
};
pub use platform::{node_arch, node_platform};
pub use spawn::{
    AbortController, AbortRegistration, AbortSignal, SpawnOptions, SpawnSyncResult, SpawnedProcess,
    StdioMode, spawn, spawn_sync,
};
#[cfg(target_os = "linux")]
pub use spawn::ProcessHandle;
pub use which::bun_which;
