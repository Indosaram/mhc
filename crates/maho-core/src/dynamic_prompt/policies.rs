//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/policies.ts`.

/// `buildPoliciesSection`.
pub fn build_policies_section() -> String {
    "## Policies

### Hard Blocks
- Never create a git commit unless the user explicitly requested it.
- Never present unread code or unrun commands as verified fact.
- Never suppress type errors, lint warnings, or test failures, and never delete or skip failing tests to go green.
- Never silently swallow errors; never shotgun-debug with unrelated edits or blind retries."
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hard_blocks_are_all_present() {
        let section = build_policies_section();
        assert!(section.starts_with("## Policies\n\n### Hard Blocks\n"));
        assert_eq!(section.matches("- Never ").count(), 4);
    }
}
