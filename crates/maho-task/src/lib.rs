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
//! checks plug in through `maho_lsp::hooks`. [`control`] adds `task_send`, the full
//! `task_cancel` contract and `/task-kill` (`docs/cleanroom/specs/task-control.md`).
pub mod agents;
pub mod control;
pub mod credentials;
pub mod extension;
pub mod ids;
pub mod isolation;
pub mod manager;
pub mod models;
pub mod record;
pub mod runner;
pub mod settings;
pub mod steering;
pub mod summary;
pub mod transport;

pub use agents::{AgentDefinition, AgentSource, Discovery, bundled_agents, discover};
pub use control::{Caller, GatewaySteer, Outcome, SteerChannel};
pub use extension::{
    AddonCommand, AddonHandler, AddonTool, KILL_COMMAND, Notifier, TOOL_NAMES, TaskAddon,
    TaskExtension, TaskScope,
};
pub use manager::{SpawnRequest, TaskManager, TaskManagerOptions};
pub use credentials::CredentialFailure;
pub use models::{FallbackPlan, ModelChoice, ModelContext, ModelRequest, choose_model};
pub use isolation::{IsolatedWorkspace, TaskObserver, WorkspaceIsolation};
pub use record::{PendingMessage, StateDir, TaskRecord, TaskStatus};
pub use runner::{CancelToken, ProcessRunner, RunOutcome, RunSpec, TaskRunner};
pub use settings::{Lineage, PARENT_SESSION_ENV, SpawnedBy, TaskSettings, current_spawned_by};
pub use summary::{format_duration, format_result, format_result_envelope, status_line};
pub use transport::{
    ChildCompletion, CompletionCapOptions, DEFAULT_COMPLETION_BATCH_LIMIT,
    DEFAULT_COMPLETION_TRANSPORT_LIMIT, MAX_COMPLETION_BATCH_LIMIT,
    MAX_COMPLETION_TRANSPORT_LIMIT, MIN_COMPLETION_BATCH_LIMIT,
    MIN_COMPLETION_TRANSPORT_LIMIT, SettledDelivery, WakeBatchOptions, WakeBatchResult,
    WakeOverflowManifest, bind_settled_batcher, build_capped_result_envelope, build_wake_batch,
    cap_completion_response, count_utf16_code_units, deliver_batch, deliver_settled,
    read_overflow_manifest, slice_utf16_bounded,
};
