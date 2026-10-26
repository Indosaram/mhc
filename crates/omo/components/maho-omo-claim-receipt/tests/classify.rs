//! Port of `claim-receipt/classify.test.ts`.

use maho_omo_claim_receipt::classify::{classify_claims, Claim, ClaimKind, ClaimStrength};

fn only(text: &str) -> Claim {
    let claims = classify_claims(text);
    assert_eq!(claims.len(), 1, "one claim for {text:?}: {claims:?}");
    claims.into_iter().next().expect("one claim")
}

fn shape(text: &str) -> (ClaimKind, ClaimStrength, bool) {
    let claim = only(text);
    (claim.kind, claim.strength, claim.admitted)
}

#[test]
fn an_english_success_claim_is_found_as_asserted() {
    assert_eq!(
        shape("All tests pass."),
        (ClaimKind::Test, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("tsc is clean."),
        (ClaimKind::Typecheck, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("No type errors."),
        (ClaimKind::Typecheck, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("The build succeeds."),
        (ClaimKind::Build, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("This already exists in `auth.ts`."),
        (ClaimKind::Exists, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("Done."),
        (ClaimKind::Done, ClaimStrength::Asserted, false)
    );
}

#[test]
fn a_korean_success_claim_is_found_without_word_boundaries() {
    assert_eq!(
        shape("테스트 모두 통과했어요."),
        (ClaimKind::Test, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("테스트통과"),
        (ClaimKind::Test, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("빌드 성공."),
        (ClaimKind::Build, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("타입 에러 없음."),
        (ClaimKind::Typecheck, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("이미 구현되어 있어요."),
        (ClaimKind::Exists, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("수정 완료했습니다."),
        (ClaimKind::Done, ClaimStrength::Asserted, false)
    );
}

#[test]
fn a_numeric_result_report_reads_zero_failures_as_a_test_claim() {
    assert_eq!(
        shape("Result: 0 failed."),
        (ClaimKind::Test, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("142/142 pass."),
        (ClaimKind::Test, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("실패 0건이에요."),
        (ClaimKind::Test, ClaimStrength::Asserted, false)
    );
    assert_eq!(
        shape("35 pass, 0 fail."),
        (ClaimKind::Test, ClaimStrength::Asserted, false)
    );
}

#[test]
fn a_report_with_nonzero_failures_is_not_a_success_claim() {
    assert!(classify_claims("3 failed, 10 passed.").is_empty());
    assert!(classify_claims("실패 2건, 통과 40건.").is_empty());
}

#[test]
fn a_hedged_claim_keeps_its_hedge() {
    assert_eq!(
        shape("Tests should pass."),
        (ClaimKind::Test, ClaimStrength::Hedged, false)
    );
    assert_eq!(
        shape("This should pass now."),
        (ClaimKind::Test, ClaimStrength::Hedged, false)
    );
    assert_eq!(
        shape("통과할 거예요."),
        (ClaimKind::Test, ClaimStrength::Hedged, false)
    );
}

#[test]
fn an_explicit_admission_marks_the_claim_admitted_even_from_the_neighbouring_sentence() {
    assert!(only("I did not run the tests; they should pass.").admitted);
    assert!(only("Tests should pass. I have not run them yet.").admitted);
    assert!(only("테스트는 아직 안 돌렸어요. 통과할 거예요.").admitted);
    assert!(only("빌드는 성공할 것 같아요 (미검증).").admitted);
}

#[test]
fn a_negated_sentence_has_no_success_claim_in_english_or_korean() {
    for text in [
        "Tests do not pass.",
        "The build isn't green yet.",
        "통과하지 못했어요.",
        "테스트가 통과하지 않았습니다.",
        "미통과 2건이 남았어요.",
        "불통과예요.",
        "아닙니다, 아직 실패합니다.",
        "아직 구현되어 있지 않아요.",
    ] {
        assert!(
            classify_claims(text).is_empty(),
            "no claim expected for {text:?}"
        );
    }
}

#[test]
fn claims_inside_code_quotes_blockquotes_or_a_relayed_report_are_ignored() {
    for text in [
        "Run `all tests pass` as the label.",
        "```\n46 pass\n0 fail\n```",
        "> All tests pass.",
        "He wrote \"all tests pass\" in the PR.",
        "그는 \u{201C}테스트 모두 통과\u{201D}라고 적었어요.",
        "그는 「빌드 성공」이라고 했어요.",
        "[REPORT] all tests pass and the build succeeds.",
    ] {
        assert!(
            classify_claims(text).is_empty(),
            "no claim expected for {text:?}"
        );
    }
}

#[test]
fn a_conditional_or_future_statement_about_a_check_is_not_a_claim() {
    for text in [
        "If the suite passes, I send READY to the lead.",
        "Its earlier review carries over once both bundle checks pass.",
        "Delta reviews get enqueued as each suite finishes green.",
        "It runs the test to show whether the failure already exists on dev.",
        "Ask: run the lane - wanted: every item merged and the prod test passed.",
        "테스트가 통과하면 머지할게요.",
    ] {
        assert!(
            classify_claims(text).is_empty(),
            "no claim expected for {text:?}"
        );
    }
}

#[test]
fn words_that_only_resemble_a_claim_are_not_claims() {
    for text in [
        "Gate v2 passed at 11:56.",
        "That alert is one I've already handled.",
        "A machine that already has memory gets the label.",
        "\u{C774}\u{BBF8} \u{AD6C}\u{D604}\u{B41C} \u{D568}\u{C218}\u{B97C} \u{C7AC}\u{C0AC}\u{C6A9}\u{D588}\u{C2B5}\u{B2C8}\u{B2E4}.",
    ] {
        assert!(
            classify_claims(text).is_empty(),
            "no claim expected for {text:?}"
        );
    }
}

#[test]
fn a_concrete_result_report_marks_the_claim_reported() {
    assert!(only("The rerun exited 0 with 343 passing.").reported);
    assert!(only("Result: 0 failed.").reported);
    assert!(only("88 related tests pass, along with typecheck.").reported);
    assert!(only("After the fix the crypto suite passes 135 of 135.").reported);
    assert!(!only("All tests pass.").reported);
    assert!(!only("Tests should pass.").reported);
}

#[test]
fn done_inside_an_ordinary_sentence_is_not_a_completion_claim() {
    for text in [
        "Once this is done we can merge.",
        "What should be done next is a review.",
        "Sign-in went through, and launch 1 of 3 is done.",
    ] {
        assert!(
            classify_claims(text).is_empty(),
            "no claim expected for {text:?}"
        );
    }
}

#[test]
fn a_multi_sentence_answer_returns_one_claim_per_claiming_sentence() {
    let claims = classify_claims("I fixed the parser.\nAll tests pass.\n빌드도 성공했어요.");
    let shape: Vec<(&'static str, &str)> = claims
        .iter()
        .map(|claim| (claim.kind.as_str(), claim.sentence.as_str()))
        .collect();
    assert_eq!(
        shape,
        vec![("test", "All tests pass."), ("build", "빌드도 성공했어요.")]
    );
}
