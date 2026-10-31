use maho_ext_rules::{
    rules::types::{MatchReason, RuleDiagnostic},
    ui::rules_banner::{
        Background, BannerRule, Foreground, RulesBanner, RulesBannerProps, StatusLineInput,
        status_line_text,
    },
};
use maho_tui::{tui::Component, utils::visible_width};
use std::{cell::RefCell, rc::Rc};

#[test]
fn empty_banner_omits_diagnostics_and_rule_rows() {
    let tones = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&tones);
    let foreground: Foreground = Rc::new(move |tone, text| {
        captured.borrow_mut().push(tone.to_owned());
        text.to_owned()
    });
    let background: Background = Rc::new(str::to_owned);
    let mut banner = RulesBanner::new(
        RulesBannerProps {
            rule_count: 0,
            diagnostics: vec![RuleDiagnostic {
                severity: "error".into(),
                source: "rule.md".into(),
                message: String::new(),
            }],
            top_rules: vec![BannerRule {
                path: None,
                relative_path: "rule.md".into(),
                match_reason: MatchReason::AlwaysApply,
            }],
        },
        foreground,
        background,
    );
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
    let mut banner = RulesBanner::new(
        RulesBannerProps {
            rule_count: 2,
            diagnostics: vec![RuleDiagnostic {
                severity: "error".into(),
                source: "bad.md".into(),
                message: String::new(),
            }],
            top_rules: vec![
                BannerRule {
                    path: None,
                    relative_path: "bad.md".into(),
                    match_reason: MatchReason::Glob {
                        pattern: "**/*.rs".into(),
                    },
                },
                BannerRule {
                    path: None,
                    relative_path: "good.md".into(),
                    match_reason: MatchReason::SingleFile,
                },
            ],
        },
        foreground,
        Rc::new(str::to_owned),
    );
    let lines = banner.render(24);
    assert_eq!(
        *tones.borrow(),
        ["accent", "dim", "error", "success", "warning"]
    );
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
    status_line_text(
        &StatusLineInput {
            rule_count: 2,
            has_errors: true,
        },
        &foreground,
    );
    assert_eq!(*tones.borrow(), ["muted", "error"]);
    tones.borrow_mut().clear();
    status_line_text(
        &StatusLineInput {
            rule_count: 0,
            has_errors: false,
        },
        &foreground,
    );
    assert_eq!(*tones.borrow(), ["muted"]);
}

#[test]
fn diagnostic_naming_the_absolute_rule_path_marks_the_row() {
    let tones = Rc::new(RefCell::new(Vec::new()));
    let captured = Rc::clone(&tones);
    let foreground: Foreground = Rc::new(move |tone, text| {
        captured.borrow_mut().push(tone.to_owned());
        text.to_owned()
    });
    let background: Background = Rc::new(str::to_owned);
    let mut banner = RulesBanner::new(
        RulesBannerProps {
            rule_count: 1,
            diagnostics: vec![RuleDiagnostic {
                severity: "error".into(),
                source: "/repo/.rules/bad.md".into(),
                message: String::new(),
            }],
            top_rules: vec![BannerRule {
                path: Some("/repo/.rules/bad.md".into()),
                relative_path: ".rules/bad.md".into(),
                match_reason: MatchReason::AlwaysApply,
            }],
        },
        foreground,
        background,
    );
    let lines = banner.render(80).join("\n");
    assert_eq!(*tones.borrow(), ["accent", "dim", "error", "warning"]);
    assert!(lines.contains("⚠ .rules/bad.md"));
}

#[test]
fn themed_banner_uses_theme_tones_and_custom_message_background() {
    let mut theme = maho_ext_api::Theme::default();
    theme.colors.insert("accent".into(), "\u{1b}[36m".into());
    theme
        .backgrounds
        .insert("customMessageBg".into(), "\u{1b}[48;5;236m".into());
    let mut banner = RulesBanner::with_theme(
        RulesBannerProps {
            rule_count: 0,
            diagnostics: vec![],
            top_rules: vec![],
        },
        &theme,
    );
    let lines = banner.render(40);
    assert!(
        lines
            .iter()
            .all(|line| line.starts_with("\u{1b}[48;5;236m"))
    );
    assert!(
        lines
            .iter()
            .any(|line| line.contains("\u{1b}[36m\u{1b}[1m[pi-rules] No rules discovered"))
    );
}
