//! Soul path constants and soul edit detection.

pub const SOUL_PATHS: &[&str] = &["system/persona.md", "system/identity.md"];
pub const SOUL_EDIT_RESULT_LINE: &str =
    "This was a soul edit: announce it to the user in your reply.";
pub const MEMORY_SOUL_EDIT_RESULT_TOKEN: &str = "soul edit";

/// Tests whether any of the affected relative paths target persona or identity soul files.
pub fn touches_soul_path(paths: &[impl AsRef<str>]) -> bool {
    paths.iter().any(|p| {
        let normalized = p.as_ref().replace('\\', "/");
        SOUL_PATHS.iter().any(|s| *s == normalized)
    })
}
