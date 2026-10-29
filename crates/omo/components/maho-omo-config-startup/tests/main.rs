//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "component.rs"]
mod component;
#[path = "faux_migration.rs"]
mod faux_migration;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "migration.rs"]
mod migration;
#[path = "notices.rs"]
mod notices;
#[path = "registered_migration_driver.rs"]
mod registered_migration_driver;
