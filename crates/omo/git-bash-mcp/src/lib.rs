//! Rust port of `@oh-my-opencode/git-bash-mcp` (SUL-1.0, internal use only).
//!
//! Serves the Windows-only `git_bash` MCP tool family: resolve Git Bash's `bash.exe` and run shell
//! commands through it. Consumes `maho-mcp-stdio-core` for JSON-RPC framing and `maho-utils` for the
//! bash resolver, re-exported here exactly as `src/git-bash-resolver.ts` and `src/index.ts` do.

pub mod cli;
pub mod mcp;
pub mod runner;

pub use mcp::DEFAULT_PROTOCOL_VERSION;
pub use mcp::DEFAULT_TIMEOUT_MS;
pub use mcp::EXEC_COMMAND_TIMEOUT_ENV_KEYS;
pub use mcp::GIT_BASH_SERVER_NAME;
pub use mcp::GIT_BASH_SERVER_VERSION;
pub use mcp::GitBashMcpOptions;
pub use mcp::GitBashServerOutcome;
pub use mcp::MAX_TIMEOUT_MS;
pub use mcp::handle_git_bash_mcp_request;
pub use mcp::run_mcp_stdio_server;
pub use mcp_stdio_core::JsonRpcResponse;
pub use runner::GitBashRunInput;
pub use runner::GitBashRunResult;
pub use runner::RunGitBashCommand;
pub use runner::run_git_bash_command;
pub use runner::run_git_bash_command_in_dir;
pub use utils::GIT_BASH_ENV_KEY;
pub use utils::GitBashResolution;
pub use utils::GitBashResolverInput;
pub use utils::GitBashSource;
pub use utils::resolve_git_bash;
pub use utils::resolve_git_bash_for_current_process;
