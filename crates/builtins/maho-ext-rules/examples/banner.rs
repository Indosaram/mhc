use std::rc::Rc;
use maho_ext_rules::{rules::types::{MatchReason, RuleDiagnostic}, ui::rules_banner::{BannerRule, RulesBanner, RulesBannerProps}};
use maho_tui::tui::Component;

fn main() {
    for width in [24, 80] {
        let mut banner = RulesBanner::new(RulesBannerProps {
            rule_count: 2,
            diagnostics: vec![RuleDiagnostic { severity: "warning".into(), source: "bad.md".into(), message: String::new() }],
            top_rules: vec![
                BannerRule { relative_path: "bad.md".into(), match_reason: MatchReason::Glob { pattern: "**/*.rs".into() } },
                BannerRule { relative_path: "good.md".into(), match_reason: MatchReason::SingleFile },
            ],
        }, Rc::new(|_, text| text.to_owned()), Rc::new(str::to_owned));
        println!("width={width}");
        for line in banner.render(width) { println!("{line}"); }
    }
}
