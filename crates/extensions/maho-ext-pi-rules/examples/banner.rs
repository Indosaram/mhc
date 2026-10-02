use maho_ext_pi_rules::{rules::types::{MatchReason,RuleDiagnostic,Severity},ui::rules_banner::*};
use maho_interactive::theme::{ColorMode,Theme,theme_json::{ColorValue,ThemeJson}};
fn main()->Result<(),Box<dyn std::error::Error>>{
    let width=std::env::args().nth(1).map_or(Ok(80),|value|value.parse::<usize>())?;
    let colors=[("border",8),("accent",6),("muted",7),("error",1),("success",2),("warning",3)].into_iter().map(|(key,value)|(key.into(),ColorValue::Index(value))).collect();
    let theme=Theme::from_json(ThemeJson{name:"fixture".into(),vars:Default::default(),colors,export_colors:Default::default()},ColorMode::Truecolor)?;
    for props in [RulesBannerProps{rule_count:0,diagnostics:Vec::new(),top_rules:None},RulesBannerProps{rule_count:2,diagnostics:vec![RuleDiagnostic{severity:Severity::Warning,source:"rules.md".into(),message:"fixture".into()}],top_rules:Some(vec![BannerRule{relative_path:"AGENTS.md".into(),path:None,match_reason:MatchReason::SingleFile},BannerRule{relative_path:"rules.md".into(),path:None,match_reason:MatchReason::Glob{pattern:"src/**".into()}}])}]{for line in render_banner_lines(&props,&theme,width){println!("{line}");}}
    println!("{}",status_line_text(&StatusLineInput{rule_count:2,has_errors:true},&theme));Ok(())
}
