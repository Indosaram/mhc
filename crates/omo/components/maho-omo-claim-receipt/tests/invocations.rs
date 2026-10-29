//! Port of `claim-receipt/invocations.test.ts`.

use maho_omo_claim_receipt::invocations::{invoked_kinds, InvokedKind};

const PREVIOUSLY_RECOGNISED: &[(&str, InvokedKind)] = &[
    ("bun test", InvokedKind::Test),
    ("bun run test", InvokedKind::Test),
    ("vitest run", InvokedKind::Test),
    ("jest --ci", InvokedKind::Test),
    ("pytest -q", InvokedKind::Test),
    ("cargo test", InvokedKind::Test),
    ("go test ./...", InvokedKind::Test),
    ("deno test", InvokedKind::Test),
    ("playwright test", InvokedKind::Test),
    ("npm test", InvokedKind::Test),
    ("npm run test", InvokedKind::Test),
    ("pnpm test", InvokedKind::Test),
    ("pnpm run test", InvokedKind::Test),
    ("yarn test", InvokedKind::Test),
    ("yarn run test", InvokedKind::Test),
    ("bun run test:senpi", InvokedKind::Test),
    ("gh pr checks 12", InvokedKind::Test),
    ("vp test", InvokedKind::Test),
    ("vp run --filter server test", InvokedKind::Test),
    ("tsc --noEmit", InvokedKind::Typecheck),
    ("tsgo --noEmit -p x", InvokedKind::Typecheck),
    ("bun run typecheck", InvokedKind::Typecheck),
    ("bun run type-check", InvokedKind::Typecheck),
    ("mypy src", InvokedKind::Typecheck),
    ("pyright", InvokedKind::Typecheck),
    ("cargo check", InvokedKind::Typecheck),
    ("biome check .", InvokedKind::Lint),
    ("eslint .", InvokedKind::Lint),
    ("oxlint", InvokedKind::Lint),
    ("ruff check", InvokedKind::Lint),
    ("cargo clippy", InvokedKind::Lint),
    ("bun run lint", InvokedKind::Lint),
    ("bun build src/index.ts", InvokedKind::Build),
    ("cargo build", InvokedKind::Build),
    ("go build ./...", InvokedKind::Build),
    ("vite build", InvokedKind::Build),
    ("next build", InvokedKind::Build),
    ("bun run build", InvokedKind::Build),
    ("npm run build", InvokedKind::Build),
    ("pnpm run build:senpi-plugin", InvokedKind::Build),
    ("yarn run build", InvokedKind::Build),
    (
        "bun packages/omo-senpi/scripts/qa/claim-receipt-e2e.mjs out",
        InvokedKind::Run,
    ),
    ("node scripts/qa/drive.mjs --self-test", InvokedKind::Run),
];

#[test]
fn every_previously_recognised_form_is_still_read_as_its_kind() {
    for (command, kind) in PREVIOUSLY_RECOGNISED {
        let kinds = invoked_kinds(command, 0);
        assert!(
            kinds.contains(kind),
            "{command} should still invoke {}, got {kinds:?}",
            kind.as_str()
        );
    }
}

#[test]
fn vp_check_covers_both_typecheck_and_lint() {
    assert_eq!(
        invoked_kinds("vp check", 0),
        vec![InvokedKind::Typecheck, InvokedKind::Lint]
    );
}

#[test]
fn the_remote_command_of_ssh_is_parsed() {
    assert_eq!(
        invoked_kinds("ssh -o BatchMode=yes -p 22 mengmota 'cd w && bun test x'", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("ssh host cd w '&&' tsgo --noEmit", 0),
        vec![InvokedKind::Typecheck]
    );
    assert_eq!(invoked_kinds("ssh host uptime", 0), Vec::new());
}

#[test]
fn command_substitution_subshells_and_loop_keywords_reach_the_check() {
    assert_eq!(
        invoked_kinds(
            "for i in 1 2; do out=$(gh pr checks 213 --repo o/r 2>/dev/null); done",
            0
        ),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("(cd w && bun test x)", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("if bun run typecheck; then echo ok; fi", 0),
        vec![InvokedKind::Typecheck]
    );
}

#[test]
fn value_taking_wrapper_flags_ssh_separators_backticks_and_here_strings_do_not_hide_the_check() {
    assert_eq!(
        invoked_kinds("env -u FOO bun test x", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("sudo -u bob bun test x", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("ssh host -- bun test x", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("x=`bun test x`", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("read a <<< foo\nbun test x", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("echo \"<<EOF\"\nbun test x", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("env -S 'bun test x'", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("sudo -c cls -R /root bun test x", 0),
        vec![InvokedKind::Test]
    );
    assert!(
        !invoked_kinds("cat > f <<'EOF' # it's the script\nbun test x\nEOF", 0)
            .contains(&InvokedKind::Test)
    );
    assert_eq!(
        invoked_kinds("echo hi # it's a note\nbun test x", 0),
        vec![InvokedKind::Test]
    );
}

#[test]
fn ci_status_readouts_count_as_test_runs() {
    assert_eq!(
        invoked_kinds("gh run view 123 --log-failed", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("gh run watch 123", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(
        invoked_kinds("gh pr view 9 --json statusCheckRollup", 0),
        vec![InvokedKind::Test]
    );
    assert_eq!(invoked_kinds("gh pr view 9 --json state", 0), Vec::new());
}
