//! Layered harness configuration: `~/.omo/omo.jsonc` plus project `.omo/omo.jsonc` files
//! (clean-room rewrite from `docs/cleanroom/specs/config.md`, see `PROVENANCE.md`).
//!
//! Loading never fails: every problem becomes a [`Diagnostic`]. Consumers read the resolved
//! [`LoadResult::view`].

pub mod catalog;
pub mod cst;
pub mod diagnostics;
pub mod env;
pub mod ext;
pub mod fs;
pub mod legacy_names;
pub mod load;
pub mod merge;
pub mod migrate;
pub mod model_string;
pub mod paths;
pub mod schema;
pub mod skills;
pub mod validate;
pub mod writer;

pub use catalog::{BUILTIN_MODEL_PROFILE_IDS, resolve_catalog_references};
pub use diagnostics::{
    Diagnostic, DiagnosticKind, DiagnosticTarget, MERGED_CONFIG, display_path, doctor_lines,
};
pub use env::{
    Env, account_home, home_dir, process_env, resolve_profile_name, user_config_dir,
    user_config_path,
};
pub use fs::{ConfigFs, RealFs};
pub use load::{Harness, Layer, LoadOptions, LoadResult, SourceRecord, load, load_native};
pub use migrate::plans::{StartupMigration, run_startup_migration, startup_migration_plans};
pub use migrate::{
    BatchOptions, BatchResult, BatchStatus, Plan, PlanMode, PlanResult, PlanStatus, run_migrations,
};
pub use model_string::{
    REASONING_LEVELS, canonical_model_string, known_level, normalize_legacy_model_fields,
};
pub use paths::{Candidate, Scope, project_paths_farthest_first, resolve_paths};
pub use skills::{collect_disabled_mcp_builtins, collect_disabled_skills};
pub use writer::{
    Edit, WriteError, WriteOp, WriteOptions, WriteOutcome, WriteTarget, update_config,
};
