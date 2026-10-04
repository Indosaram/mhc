use std::{cell::RefCell, rc::Rc};
use maho_ext_rules::{rules::types::{MatchReason, RuleDiagnostic}, ui::rules_banner::{Background, BannerRule, Foreground, RulesBanner, RulesBannerProps, StatusLineInput, status_line_text}};
use maho_tui::{tui::Component, utils::visible_width};

#[test]
fn empty_banner_omits_diagnostics_and_rule_rows() {
    let tones = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&tones);
    let foreground: Foreground = Rc::new(move |tone, text| {
        captured.borrow_mut().push(tone.to_owned());
        text.to_owned()
    });
    let background: Background = Rc::new(str::to_owned);
    let mut banner = RulesBanner::new(RulesBannerProps {
        rule_count: 0,
        diagnostics: vec![RuleDiagnostic { severity: "error".into(), source: "rule.md".into(), message: String::new() }],
        top_rules: vec![BannerRule { relative_path: "rule.md".into(), match_reason: MatchReason::AlwaysApply }],
    }, foreground, background);
    let lines = banner.render(80);
    assert_eq!(*tones.borrow(), ["accent", "dim"]);
    assert_eq!(lines.len(), 4);
    assert!(lines.iter().all(|line| visible_width(line) == 80));
}

#[test]
fn active_banner_routes_diagnostics_and_wraps_to_width() {
    let tones = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&tones);
    let foreground: Foreground = Rc::new(move |tone, text| {
        captured.borrow_mut().push(tone.to_owned());
        text.to_owned()
    });
    let mut banner = RulesBanner::new(RulesBannerProps {
        rule_count: 2,
        diagnostics: vec![RuleDiagnostic { severity: "error".into(), source: "bad.md".into(), message: String::new() }],
        top_rules: vec![
            BannerRule { relative_path: "bad.md".into(), match_reason: MatchReason::Glob { pattern: "**/*.rs".into() } },
            BannerRule { relative_path: "good.md".into(), match_reason: MatchReason::SingleFile },
        ],
    }, foreground, Rc::new(str::to_owned));
    let lines = banner.render(24);
    assert_eq!(*tones.borrow(), ["accent", "dim", "error", "success", "warning"]);
    assert!(lines.iter().all(|line| visible_width(line) == 24));
    banner.invalidate();
    assert_eq!(banner.render(24), lines);
}

#[test]
fn status_routes_errors_separately_from_muted_count() {
    let tones = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&tones);
    let foreground: Foreground = Rc::new(move |tone, text| {
        captured.borrow_mut().push(tone.to_owned());
        text.to_owned()
    });
    status_line_text(&StatusLineInput { rule_count: 2, has_errors: true }, &foreground);
    assert_eq!(*tones.borrow(), ["muted", "error"]);
    tones.borrow_mut().clear();
    status_line_text(&StatusLineInput { rule_count: 0, has_errors: false }, &foreground);
    assert_eq!(*tones.borrow(), ["muted"]);
}
