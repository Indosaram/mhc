//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "auth.rs"]
pub mod auth;
#[path = "auth_chain.rs"]
pub mod auth_chain;
#[path = "collect_images.rs"]
pub mod collect_images;
#[path = "failure.rs"]
pub mod failure;
#[path = "native_factory.rs"]
pub mod native_factory;
#[path = "params.rs"]
pub mod params;
#[path = "paths.rs"]
pub mod paths;
#[path = "reference_images.rs"]
pub mod reference_images;
#[path = "skill_gating.rs"]
pub mod skill_gating;
#[path = "tool.rs"]
pub mod tool;
#[path = "tool_2_5.rs"]
pub mod tool_2_5;
#[path = "tool_output_options.rs"]
pub mod tool_output_options;
#[path = "write_images.rs"]
pub mod write_images;

// Helpers several test modules share, declared once so each file compiles once.
#[path = "support.rs"]
pub mod support;
