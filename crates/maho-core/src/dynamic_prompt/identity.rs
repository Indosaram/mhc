//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/identity.ts`.

use super::types::SharedSections;
use crate::config::app_name;

/// `buildIdentitySection`.
pub fn build_identity_section() -> String {
    build_identity_section_for_sections(SharedSections::Diet)
}

pub fn build_identity_section_for_sections(shared_sections: SharedSections) -> String {
    match shared_sections {
        SharedSections::Full => format!(
            "You are {}, a coding agent. Your work should be indistinguishable from a careful senior engineer's.",
            app_name()
        ),
        SharedSections::Diet => format!(
            "You are {}, a coding agent. Work indistinguishable from a careful senior engineer's.",
            app_name()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_names_the_app_and_keeps_the_neutral_stance() {
        let section = build_identity_section();
        assert!(section.starts_with(&format!("You are {}, a coding agent.", app_name())));
        assert!(section.ends_with("indistinguishable from a careful senior engineer's."));
        assert!(build_identity_section_for_sections(SharedSections::Full).contains("Your work"));
    }
}
