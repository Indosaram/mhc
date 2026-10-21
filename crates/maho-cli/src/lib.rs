//! Library side of the `mhc` binary: argument parsing and command dispatch (`cli`), the package
//! manager commands (`install`/`remove`/`update`/`list`/`config`), `import-omo`, and the startup
//! config and directory migrations. `src/main.rs` is the process entry point.

pub mod cli;
pub mod import_omo;
pub mod migrations_state;
pub mod migrations;
pub mod brand_dir_migration;
pub mod legacy_senpi_dir_migration;
pub mod config;
pub mod valid_cwd;
pub mod nearest_parent_config;
pub mod utils;
pub mod extension_system_migration;
pub mod experimental;
pub mod package_manager_cli;
pub mod senpi_install;
pub mod native_update;
