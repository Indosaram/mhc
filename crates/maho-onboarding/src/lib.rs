//! First-run setup of mhc's MIT profile (spec `docs/cleanroom/specs/onboarding.md`): versioned
//! steps and their gates (ported from MIT oh-my-pi's setup wizard), an interactive line-prompt
//! flow, the non-interactive `mhc setup` path, and the `omo.jsonc` model profile edit.

pub mod apply;
pub mod cli;
pub mod flow;
pub mod host;
pub mod steps;

pub use flow::{Prompter, Scripted, Stop};
pub use host::{Choice, ProviderChoice, SetupHost};
pub use steps::{ALL_STEPS, CURRENT_SETUP_VERSION, SKIP_ENV, SelectOptions, Step, select};
