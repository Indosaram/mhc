use super::*;

fn nudged(path: &str, hint: &str) -> KibitzerNudgedRecord {
    KibitzerNudgedRecord { version: 1, nudges: vec![KibitzerNudge { path: path.into(), hint: hint.into() }], via: Some("prompt".into()) }
}

#[test]
fn given_a_nudged_record_when_rendered_then_the_title_is_the_fixed_kibitzer_line() {
    let spec = kibitzer_nudged_spec(&nudged("notes/a.md", "The note records the deploy gate.")).unwrap();
    assert_eq!(spec.glyph, "\u{2726}");
    assert_eq!(spec.tone, "accent");
    assert_eq!(spec.extra.len(), 1);
    assert_eq!(spec.extra[0].text, "notes/a.md");
}

#[test]
fn given_multiple_nudges_when_rendered_then_the_rest_are_extra_lines_with_their_paths() {
    let record = KibitzerNudgedRecord {
        version: 1,
        nudges: vec![
            KibitzerNudge { path: "a.md".into(), hint: "first hint".into() },
            KibitzerNudge { path: "b.md".into(), hint: "second hint".into() },
        ],
        via: Some("prompt".into()),
    };
    let spec = kibitzer_nudged_spec(&record).unwrap();
    assert_eq!(spec.extra.len(), 3);
    assert!(spec.extra.iter().any(|line| line.text == "a.md"));
    assert!(spec.extra.iter().any(|line| line.text == "b.md"));
}

#[test]
fn given_a_malformed_nudged_record_when_rendered_then_nothing_is_drawn() {
    assert!(kibitzer_nudged_spec(&KibitzerNudgedRecord { version: 2, nudges: vec![KibitzerNudge { path: "a".into(), hint: "h".into() }], via: None }).is_none());
    assert!(kibitzer_nudged_spec(&KibitzerNudgedRecord { version: 1, nudges: vec![], via: None }).is_none());
    assert!(kibitzer_nudged_spec(&nudged("a.md", "hint with\nnewline")).is_none());
}

#[test]
fn given_a_gate_record_when_rendered_then_the_failure_streak_and_fix_are_named() {
    let record = KibitzerGateRecord { version: 1, status: "failed".into(), cause: Some("timeout".into()), model: Some("gpt-x".into()), category: Some("quick".into()), candidate_count: 3, reason: Some("deadline".into()), run_id: Some("run-7".into()), consecutive_failures: Some(2), wake: None };
    let spec = kibitzer_gate_spec(&record).unwrap();
    assert_eq!(spec.tone, "error");
    assert_eq!(spec.extra.len(), 4);
    assert!(spec.extra.iter().any(|line| line.text.contains("categories.quick.model")));
}

#[test]
fn given_a_dropped_or_streakless_gate_record_when_rendered_then_nothing_is_drawn() {
    let dropped = KibitzerGateRecord { version: 1, status: "dropped".into(), cause: None, model: None, category: None, candidate_count: 0, reason: None, run_id: None, consecutive_failures: Some(1), wake: None };
    assert!(kibitzer_gate_spec(&dropped).is_none());
    let no_streak = KibitzerGateRecord { version: 1, status: "failed".into(), cause: None, model: None, category: None, candidate_count: 0, reason: None, run_id: None, consecutive_failures: None, wake: None };
    assert!(kibitzer_gate_spec(&no_streak).is_none());
}

#[test]
fn given_an_unavailable_record_when_rendered_then_the_fix_lists_providers() {
    let record = KibitzerUnavailableRecord { version: 1, category: "quick".into(), cause: "category_unavailable".into(), missing_providers: Some(vec!["anthropic".into(), "openai".into()]) };
    let spec = kibitzer_unavailable_spec(&record).unwrap();
    assert_eq!(spec.tone, "warning");
    assert!(spec.extra.iter().any(|line| line.text.contains("anthropic, openai")));
}

#[test]
fn given_a_bad_cause_when_rendered_then_nothing_is_drawn() {
    let record = KibitzerUnavailableRecord { version: 1, category: "quick".into(), cause: "other".into(), missing_providers: None };
    assert!(kibitzer_unavailable_spec(&record).is_none());
}
