//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "jsonc.rs"]
mod jsonc;
#[path = "loader.rs"]
mod loader;
#[path = "migration.rs"]
mod migration;
#[path = "models.rs"]
mod models;
#[path = "schema.rs"]
mod schema;
#[path = "schema_json.rs"]
mod schema_json;
#[path = "writer.rs"]
mod writer;
