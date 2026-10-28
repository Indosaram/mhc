//! Hierarchical project context files (`AGENTS.md`) for mhc's MIT profile (spec
//! `docs/cleanroom/specs/init-deep.md`): analysis, directory choice, plan, prompts, generation
//! through a model runner, the `/init-deep` command and the session-start advisor.

pub mod advisor;
pub mod analyze;
pub mod cli;
pub mod extension;
pub mod generate;
pub mod plan;
pub mod prompt;

pub use analyze::{Analysis, DirFacts, analyze, project_root};
pub use extension::ContextFilesExtension;
pub use generate::{ChildRunner, Outcome, Report, Runner, generate};
pub use plan::{Action, Plan, Settings, Target, choose, plan};
