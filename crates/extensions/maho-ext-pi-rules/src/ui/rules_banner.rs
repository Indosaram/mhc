use maho_interactive::theme::{Theme,ThemeColor};
use maho_tui::tui::Component;
use crate::rules::types::{MatchReason,RuleDiagnostic};
use super::dynamic_border::DynamicBorder;

pub struct BannerRule{pub relative_path:String,pub match_reason:MatchReason,pub path:Option<String>}
pub struct RulesBannerProps{pub rule_count:usize,pub diagnostics:Vec<RuleDiagnostic>,pub top_rules:Option<Vec<BannerRule>>}
pub struct RulesBanner{props:RulesBannerProps,theme:Theme}
impl RulesBanner{pub fn new(props:RulesBannerProps,theme:Theme)->Self{Self{props,theme}}}
impl Component for RulesBanner{fn render(&mut self,width:usize)->Vec<String>{render_banner_lines(&self.props,&self.theme,width)}}
pub fn render_banner_lines(props:&RulesBannerProps,theme:&Theme,width:usize)->Vec<String>{
    let mut border=DynamicBorder::new(|text:&str|theme.fg(ThemeColor::Border,text));
    let mut lines=border.render(width);
    let label=theme.bold(&theme.fg(ThemeColor::Accent,"[pi-rules]"));
    if props.rule_count==0{lines.push(format!("{label} No rules discovered"));lines.extend(border.render(width));return lines;}
    lines.push(format!("{label} {}",theme.fg(ThemeColor::Muted,&format!("{} active rules",props.rule_count))));lines.push(String::new());
    if let Some(rules)=&props.top_rules{for rule in rules{
        let diagnostic=props.diagnostics.iter().any(|diagnostic|rule.path.as_ref()==Some(&diagnostic.source)||diagnostic.source==rule.relative_path);
        let indicator=if diagnostic{theme.fg(ThemeColor::Error,"\u{26a0}")}else{theme.fg(ThemeColor::Success,"\u{25cf}")};
        let annotation=match &rule.match_reason{MatchReason::Glob{pattern}=>format!(" {}",theme.fg(ThemeColor::Muted,pattern)),_=>String::new()};
        lines.push(format!("  {indicator} {}{annotation}",rule.relative_path));
    }}
    if !props.diagnostics.is_empty(){lines.push(format!("  {}",theme.fg(ThemeColor::Warning,&format!("\u{26a0} {} warning(s)",props.diagnostics.len()))));}
    lines.push(String::new());lines.extend(border.render(width));lines
}
pub struct StatusLineInput{pub rule_count:usize,pub has_errors:bool}
pub fn status_line_text(input:&StatusLineInput,theme:&Theme)->String{
    let base=format!("[pi-rules] {} active",input.rule_count);
    if input.has_errors{format!("{}{}",theme.fg(ThemeColor::Muted,&format!("{base} \u{b7} ")),theme.fg(ThemeColor::Error,"\u{26a0} errors"))}else{theme.fg(ThemeColor::Muted,&base)}
}
