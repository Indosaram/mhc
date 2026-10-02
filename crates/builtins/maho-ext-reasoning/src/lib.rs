use maho_ai::types::{ThinkingLevel,ModelThinkingLevel};
use maho_core::thinking_levels::ReasoningCapability;
pub const EFFORT_LEVELS:[ThinkingLevel;6]=[ThinkingLevel::Minimal,ThinkingLevel::Low,ThinkingLevel::Medium,ThinkingLevel::High,ThinkingLevel::Xhigh,ThinkingLevel::Max];
pub fn parse_single_argument(args:&str)->Option<&str>{let args=args.trim();if args.is_empty(){return Some("");}let mut tokens=args.split_whitespace();let first=tokens.next();if tokens.next().is_some(){None}else{first}}
pub fn clamp_to_non_off(level:ModelThinkingLevel,capability:&ReasoningCapability)->Option<ThinkingLevel>{
    let levels=ModelThinkingLevel::ALL;
    let index=levels.iter().position(|candidate|*candidate==level).expect("known level");
    let supported=|candidate:ModelThinkingLevel|capability.non_off_levels.iter().copied().find(|level|ModelThinkingLevel::from(*level)==candidate);
    for candidate in levels.iter().skip(index){if let Some(level)=supported(*candidate){return Some(level);}}
    for candidate in levels[..index].iter().rev(){if let Some(level)=supported(*candidate){return Some(level);}}
    capability.non_off_levels.first().copied()
}
pub fn preferred_on_level(last_on:Option<ModelThinkingLevel>,remembered:Option<ModelThinkingLevel>,global:Option<ModelThinkingLevel>,capability:&ReasoningCapability)->Option<ThinkingLevel>{
    let non_off=|level:Option<ModelThinkingLevel>|level.filter(|level|*level!=ModelThinkingLevel::Off);
    clamp_to_non_off(non_off(last_on).or_else(||non_off(remembered)).or_else(||non_off(global)).unwrap_or(ModelThinkingLevel::Medium),capability)
}
pub fn completions<'a>(values:&'a [&str],prefix:&str)->Option<Vec<&'a str>>{let values:Vec<_>=values.iter().copied().filter(|value|value.starts_with(prefix.trim())).collect();(!values.is_empty()).then_some(values)}

