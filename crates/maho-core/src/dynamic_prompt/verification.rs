//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/verification.ts`.

/// `TestDisciplineRule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestDisciplineRule {
    pub id: &'static str,
    pub concern: &'static str,
    pub directive: &'static str,
}

/// `TEST_DISCIPLINE_RULES`.
pub const TEST_DISCIPLINE_RULES: [TestDisciplineRule; 6] = [
    TestDisciplineRule {
        id: "deterministic-tests",
        concern: "test-determinism",
        directive: "When you read or edit test code, treat nondeterminism as a bug; tests must not pass by timing luck.",
    },
    TestDisciplineRule {
        id: "fixed-wait-ban",
        concern: "async-test-orchestration",
        directive: "Unless time itself is the behavior under test, fixed sleeps, polling delays, and wait-for-time patterns are forbidden.",
    },
    TestDisciplineRule {
        id: "event-timeout-pattern",
        concern: "async-test-orchestration",
        directive: "For async behavior, subscribe to the exact event or state change before triggering the action, then await that signal with a bounded timeout.",
    },
    TestDisciplineRule {
        id: "mock-contract-integrity",
        concern: "mock-contracts",
        directive: "Mocks must preserve the behavior being asserted; do not isolate so heavily that the integration under test cannot fail.",
    },
    TestDisciplineRule {
        id: "prompt-behavior-coverage",
        concern: "prompt-tests",
        directive: "Never pin prose, prompt wording, or doc text with a test; test only machine-consumed values (parsed fields, sentinel tokens, shipped-copy equality). A pure-prose change ships with no new test.",
    },
    TestDisciplineRule {
        id: "single-pass-runner",
        concern: "test-runner",
        directive: "Run the relevant test command once and make that pass reliable; for Bun test targets, bun test must pass in a single run.",
    },
];

/// `buildTestDisciplineSection`.
pub fn build_test_discipline_section() -> String {
    let mut lines: Vec<String> = vec!["### Test Discipline".to_string()];
    for rule in TEST_DISCIPLINE_RULES {
        lines.push(format!("- {}", rule.directive));
    }
    lines.join("\n")
}

/// `buildVerificationSection`.
pub fn build_verification_section() -> String {
    format!(
        "## Verification

Tier the scope, never the rigor.

- V1 — single-file non-behavioral edits: diagnostics on that file. Done.
- V2 — single-domain behavioral edits: diagnostics on changed files in parallel, related tests, one execution of the affected runnable entry point when one exists.
- V3 — multi-file or cross-cutting work: diagnostics on every changed file, related tests, build, manual exercise of user-visible behavior through its real surface.

{}

\"Should pass\" is not verification - run the validator. Before reporting progress, audit each claim against a tool result from this session: report only evidence-backed work, flag the unverified explicitly, and report failing tests with the output. Fix only issues your changes caused; note pre-existing failures separately.",
        build_test_discipline_section()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_six_discipline_rules_keep_their_ids_and_concerns() {
        let ids: Vec<&str> = TEST_DISCIPLINE_RULES.iter().map(|rule| rule.id).collect();
        assert_eq!(
            ids,
            vec![
                "deterministic-tests",
                "fixed-wait-ban",
                "event-timeout-pattern",
                "mock-contract-integrity",
                "prompt-behavior-coverage",
                "single-pass-runner"
            ]
        );
        assert_eq!(TEST_DISCIPLINE_RULES[0].concern, "test-determinism");
        assert_eq!(TEST_DISCIPLINE_RULES[5].concern, "test-runner");
    }

    #[test]
    fn the_discipline_section_lists_every_directive_as_a_bullet() {
        let section = build_test_discipline_section();
        assert!(section.starts_with("### Test Discipline\n- "));
        assert_eq!(section.lines().count(), 7);
    }

    #[test]
    fn the_verification_section_embeds_the_discipline_rules_and_the_tiers() {
        let section = build_verification_section();
        assert!(section.starts_with("## Verification\n"));
        assert!(section.contains("### Test Discipline"));
        assert!(section.contains("- V1 — single-file non-behavioral edits"));
        assert!(section.contains("- V3 — multi-file or cross-cutting work"));
        assert!(section.ends_with("note pre-existing failures separately."));
    }
}
