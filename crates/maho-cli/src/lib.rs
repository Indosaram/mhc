//! Library side of the `mhc` binary: argument parsing and command dispatch (`cli`), the package
//! manager commands (`install`/`remove`/`update`/`list`/`config`), `import-omo`, and the startup
//! config and directory migrations. `src/main.rs` is the process entry point.
#![recursion_limit = "256"]

pub mod brand_dir_migration;
pub mod cli;
pub mod config;
pub mod experimental;
pub mod extension_system_migration;
pub mod import_omo;
pub mod internal_role;
pub mod legacy_dir_copy;
pub mod legacy_pi_edits;
pub mod legacy_senpi_dir_migration;
pub mod migrations;
pub mod migrations_state;
pub mod native_update;
pub mod nearest_parent_config;
pub mod package_manager_cli;
pub mod pi_dir_restore;
pub mod process_crash_record;
pub mod tool_child_role;
pub mod utils;
pub mod valid_cwd;
