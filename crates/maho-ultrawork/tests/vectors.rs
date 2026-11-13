//! Spec test vectors 1-32 as table tests, plus classifier edge cases.

use maho_ext_api::InputSource;
use maho_ultrawork::{
    Classification, ClassifyInput, Route, SessionArming, Stage, Suppression, classify,
    has_visible_keyword,
};

fn run(text: &str, source: InputSource) -> Classification {
    classify(ClassifyInput {
        text,
        source,
        snapshot: SessionArming::default(),
        disabled: false,
    })
}

struct Positive(&'static str, bool, bool, usize);

#[test]
fn positive_keywords_arm_with_exact_flags_and_counts() {
    let table = [
        Positive("ulw please audit the authentication module", true, false, 1),
        Positive("ULTRAWORK refactor the database connector", false, true, 1),
        Positive(
            "Use ulw for planning and ultrawork for implementation",
            true,
            true,
            2,
        ),
        Positive("프로젝트 전체를 ulw 모드로 점검해줘", true, false, 1),
        Positive("버그 원인을 ultrawork 방식으로 분석해", false, true, 1),
    ];
    for Positive(text, short, long, mentions) in table {
        let got = run(text, InputSource::Interactive);
        assert!(got.effective, "{text}");
        assert_eq!(got.short_form_seen, short, "{text}");
        assert_eq!(got.long_form_seen, long, "{text}");
        assert_eq!(got.visible_mentions, mentions, "{text}");
        assert_eq!(got.stage, Stage::Initial, "{text}");
        assert_eq!(got.route, Route::Direct, "{text}");
        assert_eq!(got.suppression, None, "{text}");
    }
}

#[test]
fn masked_regions_and_negations_never_arm() {
    let table: [(&str, Suppression); 12] = [
        (
            "Command to run: \"ulw execute\"",
            Suppression::NoVisibleKeyword,
        ),
        (
            "He recommended \u{201C}ultrawork\u{201D} during the standup",
            Suppression::NoVisibleKeyword,
        ),
        (
            "Run the script using `ulw start`",
            Suppression::NoVisibleKeyword,
        ),
        (
            "```bash\nulw test --all\n```",
            Suppression::NoVisibleKeyword,
        ),
        (
            "~~~markdown\nultrawork active\n~~~",
            Suppression::NoVisibleKeyword,
        ),
        (
            "> Note: always run ulw before deploy",
            Suppression::NoVisibleKeyword,
        ),
        (
            "Documentation: https://example.org/docs/ulw/getting-started",
            Suppression::NoVisibleKeyword,
        ),
        (
            "<custom-reminder-tag>ulw is active</custom-reminder-tag>",
            Suppression::NoVisibleKeyword,
        ),
        (
            "Please do not use ulw for this query; keep it simple",
            Suppression::ProhibitivePhrasing,
        ),
        (
            "Don't enable ultrawork mode on quick fixes",
            Suppression::ProhibitivePhrasing,
        ),
        (
            "Never run ulw on read-only repositories",
            Suppression::ProhibitivePhrasing,
        ),
        (
            "dont trigger ultrawork until tests pass",
            Suppression::ProhibitivePhrasing,
        ),
    ];
    for (text, reason) in table {
        let got = run(text, InputSource::Interactive);
        assert!(!got.effective, "{text}");
        assert_eq!(got.visible_mentions, 0, "{text}");
        assert_eq!(got.suppression, Some(reason), "{text}");
    }
}

#[test]
fn negation_separated_by_punctuation_does_not_mask_a_real_request() {
    for text in [
        "This is not working, please use ulw to fix it",
        "I can't decide; use ulw",
    ] {
        let got = run(text, InputSource::Interactive);
        assert!(got.effective, "{text}");
        assert_eq!(got.visible_mentions, 1, "{text}");
        assert_eq!(got.suppression, None, "{text}");
    }
}

#[test]
fn upstream_ascii_word_boundaries_load_negation_and_relay_headers() {
    for text in ["ulw\u{d574}\u{c918}", "\u{e9}ulw", "**ulw** go"] {
        assert!(run(text, InputSource::Interactive).effective, "{text}");
    }
    assert!(run("mass ulw\u{d574}\u{c918}", InputSource::Interactive).mass);
    let negated = run("Do not load ulw for this", InputSource::Interactive);
    assert!(!negated.effective);
    assert_eq!(negated.suppression, Some(Suppression::ProhibitivePhrasing));
    let relayed = run("[REPORT] ulw done", InputSource::Interactive);
    assert!(!relayed.effective);
    assert_eq!(relayed.suppression, Some(Suppression::NoVisibleKeyword));
    assert!(run("[Rep] ulw done", InputSource::Interactive).effective);
}

#[test]
fn quoted_ulw_inside_backticks_does_not_arm() {
    let got = run(
        "please read `\"ulw\"` in the docs",
        InputSource::Interactive,
    );
    assert!(!got.effective);
    assert_eq!(got.visible_mentions, 0);
    assert!(!has_visible_keyword("please read `\"ulw\"` in the docs"));
}

#[test]
fn embedded_directive_tags_report_route_and_reason() {
    let text =
        "Earlier turn:\n<omo-ultrawork-directive>\nbody\n</omo-ultrawork-directive>\nulw continue";
    let got = run(text, InputSource::Interactive);
    assert!(!got.effective);
    assert_eq!(got.route, Route::Embedded);
    assert_eq!(got.suppression, Some(Suppression::EmbeddedDirective));
    assert!(got.already_delivered);
}

#[test]
fn identifier_and_path_contexts_are_compound_references() {
    for text in [
        "Check the ulw-loop status in the terminal",
        "Review the plan in ulw-plan file",
        "Inspect parse_ulw_flags function",
        "Binary is at target/release/ulw.exe",
        "Pass --ulw flag to compiler",
    ] {
        let got = run(text, InputSource::Interactive);
        assert!(!got.effective, "{text}");
        assert_eq!(
            got.suppression,
            Some(Suppression::CompoundIdentifier),
            "{text}"
        );
    }
}

#[test]
fn skill_commands_and_extension_sources_route_as_specified() {
    let direct = run("/skill:ultrawork", InputSource::Interactive);
    assert!(!direct.effective);
    assert_eq!(direct.route, Route::SkillExpansion);
    assert_eq!(direct.suppression, Some(Suppression::HandledBySkill));

    let args = run(
        "/skill:reviewer ulw inspect the changes",
        InputSource::Interactive,
    );
    assert!(args.effective);
    assert_eq!(args.route, Route::SkillArguments);
    assert_eq!(args.stage, Stage::Initial);

    let bare = run(
        "/skill:reviewer inspect the changes",
        InputSource::Interactive,
    );
    assert!(!bare.effective);
    assert_eq!(bare.route, Route::None);
    assert_eq!(bare.suppression, Some(Suppression::SkillWithoutKeyword));

    let auto = run("Automated summary: ulw completed", InputSource::Extension);
    assert!(!auto.effective);
    assert_eq!(auto.suppression, Some(Suppression::ExtensionSource));
}

#[test]
fn mass_aliases_arm_and_flag_mass_mode() {
    for text in [
        "mass ulw audit security vulnerabilities",
        "Run mass-ulw on performance benchmarks",
        "Use mulw to migrate all service endpoints",
        "ulw mass optimize query latency",
        "ulw-mass refactor type definitions",
        "MASS_ULW everything",
    ] {
        let got = run(text, InputSource::Rpc);
        assert!(got.effective, "{text}");
        assert!(got.mass, "{text}");
        assert_eq!(got.visible_mentions, 1, "{text}");
    }
    assert!(!run("ulw please", InputSource::Rpc).mass);
}

#[test]
fn counts_are_independent_of_arming_state() {
    let text = "ulw and ultrawork";
    let fresh = run(text, InputSource::Interactive);
    let armed = classify(ClassifyInput {
        text,
        source: InputSource::Interactive,
        snapshot: SessionArming {
            armed: true,
            compaction_pending: false,
            revision: Some(maho_ultrawork::contract_revision()),
        },
        disabled: false,
    });
    assert_eq!(armed.stage, Stage::Reminder);
    assert_eq!(
        (
            fresh.short_form_seen,
            fresh.long_form_seen,
            fresh.visible_mentions
        ),
        (
            armed.short_form_seen,
            armed.long_form_seen,
            armed.visible_mentions
        )
    );
}

#[test]
fn disabled_flag_reports_no_effective_action() {
    let got = classify(ClassifyInput {
        text: "ulw go",
        source: InputSource::Interactive,
        snapshot: SessionArming::default(),
        disabled: true,
    });
    assert!(!got.effective);
    assert_eq!(got.suppression, Some(Suppression::Disabled));
}

/// The masking passes are skipped when the text cannot trip their detector; skipping must never
/// change what the classifier sees. Every case here carries a keyword next to a marker a pass would
/// have masked, plus the same text without the marker.
#[test]
fn gated_masking_passes_preserve_classification() {
    struct Case(&'static str, bool, usize);
    // (text, keyword still visible, visible mentions)
    let cases = [
        Case("ulw go", true, 1),
        Case("ulw plan the migration", true, 1),
        Case("ulw loop until green", true, 1),
        Case("ulw research the field", true, 1),
        Case("mass ulw", true, 1),
        Case("```\nulw in a fence\n```\nulw go", true, 1),
        Case("`ulw` inline then ulw go", true, 1),
        Case("\"quoted ulw\" then ulw go", true, 1),
        Case("> quoted ulw\nulw go", true, 1),
        Case("see https://x.dev/ulw then ulw go", true, 1),
        Case("do not load ulw now. ulw go", true, 1),
        Case("don\u{2019}t use ulw", false, 0),
        Case("doesn\u{2019}t enable ulw", false, 0),
        Case("never run ulw", false, 0),
        Case("nothing reserved here", false, 0),
    ];
    for Case(text, armed, mentions) in cases {
        for source in [InputSource::Interactive, InputSource::Rpc] {
            let got = classify(ClassifyInput {
                text,
                source,
                snapshot: SessionArming::default(),
                disabled: false,
            });
            assert_eq!(has_visible_keyword(text), armed, "armed: {text:?}");
            assert_eq!(got.visible_mentions, mentions, "mentions: {text:?}");
            assert_eq!(got.effective, armed, "effective: {text:?}");
        }
    }
}
