//! Delegated subagent tasks for mhc (MIT; see `PROVENANCE.md`).
//!
//! The task model follows the MIT oh-my-pi task tool: named agents (bundled `task` and `scout`,
//! markdown files with YAML frontmatter, the `agents` config section), unique task ids, a
//! concurrency limit, foreground and background runs whose results come back as a `<task-result>`
//! envelope, and follow-up runs that continue a task in its own session. Children run as
//! non-interactive `mhc -p` processes ([`runner::ProcessRunner`]); models come from
//! `maho_model_select::resolve_delegate_model` over the `categories`/`agents` configuration
//! (`maho-config`). Workspace isolation and team/pane presentation are traits
//! ([`isolation::WorkspaceIsolation`], [`isolation::TaskObserver`]) implemented elsewhere; post-run
//! checks plug in through `maho_lsp::hooks`.
pub mod agents;
pub mod extension;
pub mod ids;
pub mod isolation;
pub mod manager;
pub mod models;
pub mod record;
pub mod runner;
pub mod settings;
pub mod summary;

pub use agents::{AgentDefinition, AgentSource, Discovery, bundled_agents, discover};
pub use extension::{AddonCommand, AddonHandler, AddonTool, Notifier, TOOL_NAMES, TaskAddon, TaskExtension, TaskScope};
pub use manager::{SpawnRequest, TaskManager, TaskManagerOptions};
pub use models::{ModelChoice, ModelContext, ModelRequest, choose_model};
pub use isolation::{IsolatedWorkspace, TaskObserver, WorkspaceIsolation};
pub use record::{StateDir, TaskRecord, TaskStatus};
pub use runner::{CancelToken, ProcessRunner, RunOutcome, RunSpec, TaskRunner};
pub use settings::TaskSettings;
