//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/policies.ts`.

use super::types::SharedSections;

pub const DEFAULT_ERROR_RULE: &str =
    "Never swallow errors silently; no shotgun debugging (unrelated edits, blind retries).";
const FULL_ERROR_RULE: &str =
    "Never silently swallow errors; never shotgun-debug with unrelated edits or blind retries.";

/// `buildPoliciesSection`.
pub fn build_policies_section() -> String {
    build_policies_section_with_options(None, SharedSections::Diet)
}

pub fn build_policies_section_with_options(
    error_rule: Option<&str>,
    shared_sections: SharedSections,
) -> String {
    match shared_sections {
        SharedSections::Full => format!(
            "## Policies

### Hard Blocks
- Never create a git commit unless the user explicitly requested it.
- Never present unread code or unrun commands as verified fact.
- Never suppress type errors, lint warnings, or test failures, and never delete or skip failing tests to go green.
- {}
- Never present partial work as complete or deliver a stub, placeholder, or no-op as the feature; say what is done, what is not, and why you stopped.",
            error_rule.unwrap_or(FULL_ERROR_RULE)
        ),
        SharedSections::Diet => format!(
            "## Hard Limits

- No git commit unless user explicitly asked.
- Never suppress type errors, lint warnings, test failures; never delete/skip failing tests to go green.
- {}
- Never present partial work as complete; no stub, placeholder, or no-op shipped as the feature.",
            error_rule.unwrap_or(DEFAULT_ERROR_RULE)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hard_blocks_are_all_present() {
        let diet = build_policies_section();
        assert!(diet.starts_with("## Hard Limits\n\n"));
        assert_eq!(diet.matches("\n- ").count(), 4);

        let full = build_policies_section_with_options(None, SharedSections::Full);
        assert!(full.starts_with("## Policies\n\n### Hard Blocks\n"));
        assert_eq!(full.matches("- Never ").count(), 5);
    }
}
