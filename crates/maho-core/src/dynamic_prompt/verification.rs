//! Port of senpi `packages/coding-agent/src/core/dynamic-prompt/verification.ts`.

use super::PromptSurface;
use super::types::SharedSections;

/// `TestDisciplineRule`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestDisciplineRule {
    pub id: &'static str,
    pub concern: &'static str,
    pub directive: &'static str,
    pub full_directive: &'static str,
}

/// `TEST_DISCIPLINE_RULES`.
pub const TEST_DISCIPLINE_RULES: [TestDisciplineRule; 6] = [
    TestDisciplineRule {
        id: "deterministic-tests",
        concern: "test-determinism",
        directive: "Reading/editing test code: nondeterminism = bug; no passing by timing luck.",
        full_directive: "When you read or edit test code, treat nondeterminism as a bug; tests must not pass by timing luck.",
    },
    TestDisciplineRule {
        id: "fixed-wait-ban",
        concern: "async-test-orchestration",
        directive: "Fixed sleeps, polling delays, wait-for-time: forbidden unless time itself is under test.",
        full_directive: "Unless time itself is the behavior under test, fixed sleeps, polling delays, and wait-for-time patterns are forbidden.",
    },
    TestDisciplineRule {
        id: "event-timeout-pattern",
        concern: "async-test-orchestration",
        directive: "Async: subscribe to the exact event/state change before triggering, then await it with a bounded timeout.",
        full_directive: "For async behavior, subscribe to the exact event or state change before triggering the action, then await that signal with a bounded timeout.",
    },
    TestDisciplineRule {
        id: "mock-contract-integrity",
        concern: "mock-contracts",
        directive: "Mocks keep the asserted behavior; never isolate so hard the integration cannot fail.",
        full_directive: "Mocks must preserve the behavior being asserted; do not isolate so heavily that the integration under test cannot fail.",
    },
    TestDisciplineRule {
        id: "prompt-behavior-coverage",
        concern: "prompt-tests",
        directive: "Never pin prose, prompt wording, or doc text with a test; test machine-consumed values only (parsed fields, sentinel tokens, shipped-copy equality). Pure-prose change = no new test.",
        full_directive: "Never pin prose, prompt wording, or doc text with a test; test only machine-consumed values (parsed fields, sentinel tokens, shipped-copy equality). A pure-prose change ships with no new test.",
    },
    TestDisciplineRule {
        id: "single-pass-runner",
        concern: "test-runner",
        directive: "Run the relevant test command once, make that pass reliable; Bun targets: bun test passes in a single run.",
        full_directive: "Run the relevant test command once and make that pass reliable; for Bun test targets, bun test must pass in a single run.",
    },
];

/// `buildTestDisciplineSection`.
pub fn build_test_discipline_section() -> String {
    build_test_discipline_section_for_sections(SharedSections::Diet)
}

pub fn build_test_discipline_section_for_sections(shared_sections: SharedSections) -> String {
    let mut lines: Vec<String> = vec!["### Test Discipline".to_string()];
    for rule in TEST_DISCIPLINE_RULES {
        let text = match shared_sections {
            SharedSections::Full => rule.full_directive,
            SharedSections::Diet => rule.directive,
        };
        lines.push(format!("- {text}"));
    }
    lines.join("\n")
}

/// `APP_UNRUN_CHECK_RULE` (senpi#2377): the app surface's claim audit. Every app-surface
/// verification rule renders it.
pub const APP_UNRUN_CHECK_RULE: &str = "A check that did not run is covered by the evidence that did run; name it only when no other evidence supports the claim. Replies render in an app, so tool and hook feedback (comment-checker findings, language-server availability, internal notices) is for you to act on: it reaches the user only when it changes what they get, and an unavailable tool or hook never does by itself.";

/// `CLAIM_AUDIT[terminalOrApp(surface)]`: chat takes the app wording.
fn claim_audit(surface: PromptSurface, shared_sections: SharedSections) -> String {
    match (surface, shared_sections) {
        (PromptSurface::Terminal, SharedSections::Diet) => {
            "evidence-backed work only, flag unverified explicitly, failing tests with output.".to_owned()
        }
        (PromptSurface::App | PromptSurface::Chat, SharedSections::Diet) => {
            format!("evidence-backed work only, failing tests with output. {APP_UNRUN_CHECK_RULE}")
        }
        (PromptSurface::Terminal, SharedSections::Full) => {
            "report only evidence-backed work, flag the unverified explicitly, and report failing tests with the output.".to_owned()
        }
        (PromptSurface::App | PromptSurface::Chat, SharedSections::Full) => {
            format!("report only evidence-backed work and report failing tests with the output. {APP_UNRUN_CHECK_RULE}")
        }
    }
}

/// `buildVerificationSection(options)`: `surface` selects the claim audit.
pub fn build_verification_section(surface: PromptSurface) -> String {
    build_verification_section_for_sections(surface, SharedSections::Diet)
}

pub fn build_verification_section_for_sections(
    surface: PromptSurface,
    shared_sections: SharedSections,
) -> String {
    let discipline = build_test_discipline_section_for_sections(shared_sections);
    let audit = claim_audit(surface, shared_sections);
    match shared_sections {
        SharedSections::Full => format!(
            "## Verification

Tier the scope, never the rigor.

- V1 (single-file non-behavioral edits): diagnostics on that file. Done.
- V2 (single-domain behavioral edits): diagnostics on changed files in parallel, related tests, one execution of the affected runnable entry point when one exists.
- V3 (multi-file or cross-cutting work): diagnostics on every changed file, related tests, build, manual exercise of user-visible behavior through its real surface.

{discipline}

\"Should pass\" is not verification - run the validator. Before reporting progress, audit each claim against a tool result from this session: {audit} Fix failures your change caused and any that sit in the code you changed; note other pre-existing failures separately."
        ),
        SharedSections::Diet => format!(
            "## Verification

Tier the scope, never the rigor.

- V1 single-file non-behavioral edit: diagnostics on that file. Done.
- V2 single-domain behavioral edit: diagnostics on changed files in parallel + related tests + one run of affected runnable entry point if any.
- V3 multi-file / cross-cutting: diagnostics on every changed file + related tests + build + manual exercise of user-visible behavior through its real surface.

{discipline}

Before reporting progress: audit each claim against a tool result from this session; {audit} Fix failures your change caused and any that sit in the code you changed; note other pre-existing failures separately."
        ),
    }
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
        let section = build_verification_section(PromptSurface::Terminal);
        assert!(section.starts_with("## Verification\n"));
        assert!(section.contains("### Test Discipline"));
        assert!(section.contains("- V1 single-file non-behavioral edit"));
        assert!(section.contains("- V3 multi-file / cross-cutting"));
        assert!(section.ends_with("note other pre-existing failures separately."));
    }

    /// senpi#2377: every app-surface verification rule renders `APP_UNRUN_CHECK_RULE`, and chat takes
    /// the app wording (`terminalOrApp`).
    #[test]
    fn the_app_and_chat_claim_audits_render_the_unrun_check_rule() {
        for surface in [PromptSurface::App, PromptSurface::Chat] {
            let section = build_verification_section(surface);
            assert!(section.contains(APP_UNRUN_CHECK_RULE));
            assert!(section.contains("evidence-backed work only, failing tests with output."));
        }
        let terminal = build_verification_section(PromptSurface::Terminal);
        assert!(!terminal.contains(APP_UNRUN_CHECK_RULE));
        assert!(terminal.contains("flag unverified explicitly"));
    }
}
