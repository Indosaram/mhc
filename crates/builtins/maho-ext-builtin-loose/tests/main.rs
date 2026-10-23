//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "account_display_name.rs"]
mod account_display_name;
#[path = "files.rs"]
mod files;
#[path = "files_patch.rs"]
mod files_patch;
#[path = "gpt_account.rs"]
mod gpt_account;
#[path = "import_repro.rs"]
mod import_repro;
#[path = "monitor.rs"]
mod monitor;
#[path = "oauth_login_interaction.rs"]
mod oauth_login_interaction;
#[path = "prompt_url_widget.rs"]
mod prompt_url_widget;
#[path = "redraws.rs"]
mod redraws;
#[path = "service_tier.rs"]
mod service_tier;
#[path = "tps.rs"]
mod tps;
