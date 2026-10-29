//! Team spec discovery, normalization, validation and loading.

pub mod loader;
pub mod paths;
pub mod team_spec_input_normalizer;
pub mod validator;

pub use loader::{LoadedTeamSpec, load_all_team_specs, load_team_spec};
pub use paths::*;
pub use team_spec_input_normalizer::{NormalizeTeamSpecInputOptions, normalize_team_spec_input};
pub use validator::{validate_dual_support, validate_member_eligibility, validate_spec};
