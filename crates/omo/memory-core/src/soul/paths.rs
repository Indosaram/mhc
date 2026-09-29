//! Soul file identity and result directives for memory modifications.

/// Canonical soul file paths monitored for persona and identity mutations.
pub const SOUL_PATHS: [&str; 2] = ["system/persona.md", "system/identity.md"];

/// Stable machine-consumed token embedded in tool result soul-edit directives.
pub const MEMORY_SOUL_EDIT_RESULT_TOKEN: &str = "soul edit";

/// Model-facing discipline line appended to tool results on a soul edit.
pub const SOUL_EDIT_RESULT_LINE: &str =
    "This was a soul edit: announce it to the user in your reply.";

/// Returns true if any of the provided paths match a soul path.
pub fn touches_soul_path<S: AsRef<str>>(paths: &[S]) -> bool {
    paths.iter().any(|p| SOUL_PATHS.contains(&p.as_ref()))
}

#[cfg(test)]
#[path = "paths_tests.rs"]
mod tests;
