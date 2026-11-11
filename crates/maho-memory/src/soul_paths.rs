//! The files that are the agent's self-model, and the notice an edit to one must carry.
//!
//! Spec: `docs/cleanroom/specs/memory-soul-paths.md`. Three files describe who the agent is. An edit to one is
//! not an ordinary memory write: it changes the agent's identity, so the person should hear about it in the
//! reply rather than discover it in a diff.

pub const SOUL_PATHS: [&str; 3] = [
    "system/persona.md",
    "system/identity.md",
    "system/boundaries.md",
];

pub const MEMORY_SOUL_EDIT_RESULT_TOKEN: &str = "soul edit";

pub const SOUL_EDIT_RESULT_LINE: &str =
    "This was a soul edit: announce it to the user in your reply.";

/// Whether any of `paths` is a soul file.
///
/// Compared against the canonical paths exactly, not by suffix: `notes/system/persona.md` is a note that
/// happens to be named persona, not the persona.
pub fn touches_soul_path<S: AsRef<str>>(paths: &[S]) -> bool {
    paths.iter().any(|path| SOUL_PATHS.contains(&path.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_soul_file_is_recognised() {
        for path in SOUL_PATHS {
            assert!(touches_soul_path(&[path]), "{path}");
        }
    }

    #[test]
    fn an_ordinary_memory_file_is_not_a_soul_file() {
        assert!(!touches_soul_path(&[
            "system/human.md",
            "system/self-aware.md",
            "notes/a.md"
        ]));
    }

    #[test]
    fn one_soul_file_among_many_is_enough() {
        assert!(touches_soul_path(&["notes/a.md", "system/persona.md"]));
    }

    #[test]
    fn a_file_merely_named_persona_is_not_the_persona() {
        assert!(!touches_soul_path(&[
            "notes/system/persona.md",
            "people/x/system/persona.md"
        ]));
    }

    #[test]
    fn no_paths_touches_nothing() {
        assert!(!touches_soul_path::<&str>(&[]));
    }

    #[test]
    fn the_result_token_is_stable() {
        assert_eq!(MEMORY_SOUL_EDIT_RESULT_TOKEN, "soul edit");
        assert!(SOUL_EDIT_RESULT_LINE.contains("announce it to the user"));
    }
}
