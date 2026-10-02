pub use crate::types::RequestedInterval;
use crate::types::RequestedIntervalUnit;
use regex::Regex;
use std::sync::OnceLock;
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoopTarget { All, Id(String), Implicit }
#[derive(Clone, Debug, PartialEq)]
pub enum ParsedLoopInvocation {
    Stop { target: LoopTarget, original_args: String }, Status { original_args: String },
    Pause { target: LoopTarget, original_args: String }, Resume { target: LoopTarget, original_args: String },
    Fixed { interval: RequestedInterval, prompt: String, original_args: String },
    Dynamic { prompt: String, original_args: String }, Bare { interval: Option<RequestedInterval>, original_args: String },
    Invalid { reason: String, usage: String },
}
const USAGE: &str = "Usage: /loop [interval] <prompt>\nExamples:\n  /loop 5m check the deploy\n  /loop check the deploy every 20m\n  /loop check the deploy\n  /loop stop [id|all]\n  /loop status\n  /loop pause [id|all]\n  /loop resume [id|all]";
fn js_whitespace(c: char) -> bool { matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}' | '\u{feff}') }
fn trim(value: &str) -> &str { value.trim_matches(js_whitespace) }
fn target(rest: &str) -> LoopTarget {
    let rest = trim(rest);
    match rest.to_lowercase().as_str() { "" | "implicit" => LoopTarget::Implicit, "all" => LoopTarget::All, _ => LoopTarget::Id(rest.split(js_whitespace).next().unwrap_or("").into()) }
}
fn unit(value: &str) -> RequestedIntervalUnit { match value.to_lowercase().chars().next() { Some('s') => RequestedIntervalUnit::Seconds, Some('m') => RequestedIntervalUnit::Minutes, Some('h') => RequestedIntervalUnit::Hours, Some('d') | None | Some(_) => RequestedIntervalUnit::Days } }
fn classify(interval: RequestedInterval, remaining: &str, original_args: &str) -> ParsedLoopInvocation {
    if interval.value.abs() < f64::MIN_POSITIVE { return ParsedLoopInvocation::Invalid { reason: "Interval amount must be greater than zero.".into(), usage: USAGE.into() }; }
    let prompt = trim(remaining);
    if prompt.is_empty() { ParsedLoopInvocation::Bare { interval: Some(interval), original_args: original_args.into() } }
    else { ParsedLoopInvocation::Fixed { interval, prompt: prompt.into(), original_args: original_args.into() } }
}
fn expression(cell: &'static OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| match Regex::new(pattern) { Ok(regex) => regex, Err(error) => panic!("invalid static loop parser expression: {error}") })
}
pub fn parse_loop_args(raw: &str) -> ParsedLoopInvocation {
    let trimmed = trim(raw);
    let original_args = raw.to_owned();
    if !trimmed.is_empty() {
        let first = trimmed.split(js_whitespace).next().unwrap_or("").to_lowercase();
        match first.as_str() {
            "stop" => return ParsedLoopInvocation::Stop { target: target(&trimmed[first.len()..]), original_args },
            "status" => return ParsedLoopInvocation::Status { original_args },
            "pause" => return ParsedLoopInvocation::Pause { target: target(&trimmed[first.len()..]), original_args },
            "resume" => return ParsedLoopInvocation::Resume { target: target(&trimmed[first.len()..]), original_args },
            _ => (),
        }
    }
    static LEADING: OnceLock<Regex> = OnceLock::new();
    static TRAILING: OnceLock<Regex> = OnceLock::new();
    let whitespace = "[\\x{0009}-\\x{000d}\\x{0020}\\x{00a0}\\x{1680}\\x{2000}-\\x{200a}\\x{2028}\\x{2029}\\x{202f}\\x{205f}\\x{3000}\\x{feff}]";
    let leading = expression(&LEADING, &format!("^{whitespace}*([0-9]+[smhd])(?-u:\\b)"));
    if let Some(captures) = leading.captures(raw)
        && let (Some(full), Some(token)) = (captures.get(0), captures.get(1)) {
        let token = token.as_str();
        let value = token[..token.len() - 1].parse::<f64>().unwrap_or(f64::INFINITY);
        return classify(RequestedInterval { value, unit: unit(&token[token.len() - 1..]), raw: token.into() }, &raw[full.end()..], raw);
    }
    let trailing = expression(&TRAILING, &format!("(?i)(?:^|{whitespace})every{whitespace}+([0-9]+){whitespace}*(s|sec|secs|second|seconds|m|min|mins|minute|minutes|h|hr|hrs|hour|hours|d|day|days){whitespace}*$"));
    if let Some(captures) = trailing.captures(raw)
        && let (Some(full), Some(amount), Some(suffix)) = (captures.get(0), captures.get(1), captures.get(2)) {
        let value = amount.as_str().parse::<f64>().unwrap_or(f64::INFINITY);
        let unit = unit(suffix.as_str());
        let suffix = match unit { RequestedIntervalUnit::Seconds => "s", RequestedIntervalUnit::Minutes => "m", RequestedIntervalUnit::Hours => "h", RequestedIntervalUnit::Days => "d" };
        return classify(RequestedInterval { value, unit, raw: format!("{}{suffix}",maho_ai::utils::js::number_to_string(value)) }, &raw[..full.start()], raw);
    }
    if trimmed.is_empty() { ParsedLoopInvocation::Bare { interval: None, original_args } }
    else { ParsedLoopInvocation::Dynamic { prompt: trimmed.into(), original_args } }
}
#[cfg(test)] mod tests {
    use super::*;
    fn fixed(raw: &str, amount: f64, suffix: RequestedIntervalUnit, prompt: &str) { let result = parse_loop_args(raw); let ParsedLoopInvocation::Fixed { interval, prompt: actual, original_args } = result else { panic!("expected fixed loop"); }; assert!((interval.value - amount).abs() < f64::EPSILON); assert_eq!(interval.unit, suffix); assert_eq!(actual, prompt); assert_eq!(original_args, raw); }
    #[test] fn leading_interval_token() { fixed("5m /babysit-prs", 5.0, RequestedIntervalUnit::Minutes, "/babysit-prs"); }
    #[test] fn trailing_every_clause() { fixed("check the deploy every 20m", 20.0, RequestedIntervalUnit::Minutes, "check the deploy"); }
    #[test] fn trailing_unit_word() { fixed("run tests every 5 minutes", 5.0, RequestedIntervalUnit::Minutes, "run tests"); }
    #[test] fn no_interval_is_dynamic() { let result = parse_loop_args("check the deploy"); assert!(matches!(result, ParsedLoopInvocation::Dynamic { prompt, .. } if prompt == "check the deploy")); }
    #[test] fn every_pr_is_not_interval() { let result = parse_loop_args("check every PR"); assert!(matches!(result, ParsedLoopInvocation::Dynamic { .. })); }
    #[test] fn interval_only_is_bare() { let result = parse_loop_args("5m"); assert!(matches!(result, ParsedLoopInvocation::Bare { interval: Some(_), .. })); }
    #[test] fn empty_is_bare() { let result = parse_loop_args(""); assert!(matches!(result, ParsedLoopInvocation::Bare { interval: None, .. })); }
    #[test] fn leading_zero_is_invalid() { let result = parse_loop_args("0m x"); assert!(matches!(result, ParsedLoopInvocation::Invalid { .. })); }
    #[test] fn stop_implicit() { let result = parse_loop_args("stop"); assert!(matches!(result, ParsedLoopInvocation::Stop { target: LoopTarget::Implicit, .. })); }
    #[test] fn stop_all() { let result = parse_loop_args("stop all"); assert!(matches!(result, ParsedLoopInvocation::Stop { target: LoopTarget::All, .. })); }
    #[test] fn stop_id() { let result = parse_loop_args("stop 84dabc"); assert!(matches!(result, ParsedLoopInvocation::Stop { target: LoopTarget::Id(id), .. } if id == "84dabc")); }
    #[test] fn status() { let result = parse_loop_args("status"); assert!(matches!(result, ParsedLoopInvocation::Status { .. })); }
    #[test] fn pause_implicit() { let result = parse_loop_args("pause"); assert!(matches!(result, ParsedLoopInvocation::Pause { target: LoopTarget::Implicit, .. })); }
    #[test] fn pause_id() { let result = parse_loop_args("pause 84dabc"); assert!(matches!(result, ParsedLoopInvocation::Pause { target: LoopTarget::Id(id), .. } if id == "84dabc")); }
    #[test] fn pause_all() { let result = parse_loop_args("pause all"); assert!(matches!(result, ParsedLoopInvocation::Pause { target: LoopTarget::All, .. })); }
    #[test] fn resume_implicit() { let result = parse_loop_args("resume"); assert!(matches!(result, ParsedLoopInvocation::Resume { target: LoopTarget::Implicit, .. })); }
    #[test] fn resume_id() { let result = parse_loop_args("resume 84dabc"); assert!(matches!(result, ParsedLoopInvocation::Resume { target: LoopTarget::Id(id), .. } if id == "84dabc")); }
    #[test] fn resume_all() { let result = parse_loop_args("resume all"); assert!(matches!(result, ParsedLoopInvocation::Resume { target: LoopTarget::All, .. })); }
    #[test] fn inner_spacing_preserved() { fixed("2h  spaced   prompt", 2.0, RequestedIntervalUnit::Hours, "spaced   prompt"); }
    #[test] fn leading_wins_over_trailing() { fixed("5m check every 20m", 5.0, RequestedIntervalUnit::Minutes, "check every 20m"); }
    #[test] fn case_and_punctuation_preserved() { fixed("1d Check THE Deploy!", 1.0, RequestedIntervalUnit::Days, "Check THE Deploy!"); }
    #[test] fn trailing_zero_is_invalid() { let result = parse_loop_args("x every 0 seconds"); assert!(matches!(result, ParsedLoopInvocation::Invalid { .. })); }
    #[test] fn all_unit_aliases_are_accepted() { for (aliases, unit) in [("s sec secs second seconds", RequestedIntervalUnit::Seconds), ("m min mins minute minutes", RequestedIntervalUnit::Minutes), ("h hr hrs hour hours", RequestedIntervalUnit::Hours), ("d day days", RequestedIntervalUnit::Days)] { for alias in aliases.split(' ') { fixed(&format!("task every 1{alias}"), 1.0, unit, "task"); } } }
    #[test] fn original_arguments_are_preserved() { let raw = "  status  "; let result = parse_loop_args(raw); assert!(matches!(result, ParsedLoopInvocation::Status { original_args } if original_args == raw)); }
    #[test] fn trailing_large_intervals_use_javascript_number_labels() {
        for (amount,expected) in [("1000000000000000000000","1e+21m"),("9007199254740993","9007199254740992m")] {
            let ParsedLoopInvocation::Fixed { interval,.. }=parse_loop_args(&format!("work every {amount} minutes")) else { panic!("expected fixed interval"); };
            assert_eq!(interval.raw,expected);
        }
    }
    #[test] fn overflowing_interval_preserves_javascript_infinity() {
        let ParsedLoopInvocation::Fixed { interval,.. }=parse_loop_args(&format!("work every {} seconds","9".repeat(400))) else { panic!("expected fixed interval"); };
        assert_eq!(interval.value,f64::INFINITY); assert_eq!(interval.raw,"Infinitys");
    }
}
