//! Port of senpi `packages/agent/src/harness/` (todo 15).
//!
//! One senpi source file maps to one Rust module here, keeping names and order so an upstream
//! diff translates mechanically.

pub mod agent_harness;
pub mod compaction;
pub mod config;
pub mod context;
pub mod env;
pub mod events;
pub mod execution;
pub mod hooks;
pub mod messages;
pub mod prompt_templates;
pub mod result;
pub mod runtime;
pub mod session;
pub mod skills;
pub mod system_prompt;
pub mod telemetry;
pub mod tools;
pub mod types;
pub mod utils;

pub use result::{
    Closed, HarnessClosed, HarnessFault, InvalidLane, InvalidMessage, InvalidNavigation, LaneBusy,
    NoActiveOperation, NoActiveRun, NothingToCompact, NothingToResume, OperationKind, OperationMismatch,
    TaggedErrorValue, UnknownSkill, UnknownTarget, UnknownTemplate,
};
pub use types::{
    AgentHarnessResources, AgentHarnessStreamOptions, AgentHarnessStreamOptionsPatch, AgentHarnessTool,
    AgentHarnessToolContextSource, AgentHarnessToolInvocation, AgentHarnessToolUpdateCallback,
    AgentHarnessToolUpdateOptions, BranchSummaryError, CompactionError, ExecutionEnv, ExecutionError,
    FileError, FileErrorCode, FileInfo, FileKind, FileSystem, PromptTemplate, Shell, ShellExecOptions,
    ShellExecResult, ShellOutputCaptureOptions, ShellOutputLimits, ShellOutputMetadata, ShellOutputRetention,
    ShellOutputTruncation, ShellOutputUpdate, ShellOutputView, Skill, TextLine, TextLineReader,
};
