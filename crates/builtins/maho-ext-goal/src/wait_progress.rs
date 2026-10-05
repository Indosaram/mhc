use std::collections::BTreeMap;
use crate::cache_warm::format_wake_duration;
pub type ResumptionChannelCounts=BTreeMap<String,f64>;
pub const GOAL_WAIT_BAR_CELLS:usize=12;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum GoalWaitKind { Monitor,UserGrace }
pub struct GoalWaitLabelInput { pub kind:GoalWaitKind,pub remaining_ms:f64,pub total_ms:f64,pub channel_counts:ResumptionChannelCounts }
fn clamp_ratio(value:f64)->f64 { if value.is_finite() { value.clamp(0.0,1.0) } else { 0.0 } }
pub fn render_goal_wait_bar(ratio:f64)->String { let filled=(clamp_ratio(ratio)*GOAL_WAIT_BAR_CELLS as f64+0.5).floor() as usize; format!("{}{}","\u{25b0}".repeat(filled),"\u{25b1}".repeat(GOAL_WAIT_BAR_CELLS-filled)) }
fn source_index(source:&str)->usize { ["terminal-monitors","senpi-task","senpi-codemode","terminal-background-sessions"].iter().position(|item|*item==source).unwrap_or(4) }
fn source_count(source:&str,count:f64)->String {
    let number=maho_ai::utils::js::number_to_string(count);
    let label=match source { "terminal-monitors"=>if count==1.0 { "wake source" } else { "wake sources" },"senpi-task"=>if count==1.0 { "task" } else { "tasks" },"senpi-codemode"=>if count==1.0 { "eval" } else { "evals" },"terminal-background-sessions"=>if count==1.0 { "bash" } else { "bash sessions" },_=>return format!("{number} {}{}",source.replace('-'," "),if count==1.0 { "" } else { " channels" }) }; format!("{number} {label}")
}
pub fn channels_on_duty(counts:&ResumptionChannelCounts)->String {
    let collator=icu_collator::Collator::try_new(Default::default(),Default::default()).expect("compiled collation data is available");
    let mut sources=counts.iter().filter(|(_,count)|**count>0.0).collect::<Vec<_>>(); sources.sort_by(|(left,_),(right,_)|source_index(left).cmp(&source_index(right)).then_with(||collator.compare(left,right)));
    format!("{} on duty",sources.into_iter().map(|(source,count)|source_count(source,*count)).collect::<Vec<_>>().join(" \u{b7} "))
}
pub fn format_goal_wait_label(input:&GoalWaitLabelInput)->String {
    let ratio=if input.total_ms<=0.0 { 1.0 } else { clamp_ratio((input.total_ms-input.remaining_ms.max(0.0))/input.total_ms) };
    let bar=render_goal_wait_bar(ratio); let remaining=format_wake_duration(input.remaining_ms.max(0.0));
    match input.kind { GoalWaitKind::Monitor=>format!("{bar} goal continues in {remaining} \u{b7} {}",channels_on_duty(&input.channel_counts)),GoalWaitKind::UserGrace=>format!("{bar} goal resumes in {remaining}") }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn unknown_sources_use_locale_collation_instead_of_codepoint_order() {
        let counts=BTreeMap::from([("zeta".into(),1.0),("éclair".into(),1.0),("Alpha".into(),1.0),("alpha".into(),1.0)]);
        assert_eq!(channels_on_duty(&counts),"1 alpha \u{b7} 1 Alpha \u{b7} 1 éclair \u{b7} 1 zeta on duty");
    }
    #[test] fn bar_clamps_and_rounds_to_twelve_cells() { let result=[render_goal_wait_bar(-1.0),render_goal_wait_bar(0.5),render_goal_wait_bar(2.0),render_goal_wait_bar(f64::NAN)]; assert_eq!(result[0].chars().filter(|c|*c=='\u{25b0}').count(),0); assert_eq!(result[1].chars().filter(|c|*c=='\u{25b0}').count(),6); assert_eq!(result[2].chars().filter(|c|*c=='\u{25b0}').count(),12); assert_eq!(result[3].chars().count(),12); }
    #[test] fn upstream_empty_bar() { assert_eq!(render_goal_wait_bar(0.0),"\u{25b1}".repeat(12)); }
    #[test] fn upstream_full_bar() { assert_eq!(render_goal_wait_bar(1.0),"\u{25b0}".repeat(12)); }
    #[test] fn upstream_halfway_bar() { assert_eq!(render_goal_wait_bar(0.5),format!("{}{}","\u{25b0}".repeat(6),"\u{25b1}".repeat(6))); }
    #[test] fn upstream_out_of_range_bars() { assert_eq!(render_goal_wait_bar(-1.0),render_goal_wait_bar(0.0)); assert_eq!(render_goal_wait_bar(9.0),render_goal_wait_bar(1.0)); }
    #[test] fn upstream_user_grace_partial_wait() { let label=format_goal_wait_label(&GoalWaitLabelInput { kind:GoalWaitKind::UserGrace,remaining_ms:47000.0,total_ms:60000.0,channel_counts:Default::default() }); assert!(label.contains("47s")); assert!(label.contains('\u{25b0}')); assert!(label.contains('\u{25b1}')); }
    #[test] fn upstream_cache_budget_monitor_wait() { let label=format_goal_wait_label(&GoalWaitLabelInput { kind:GoalWaitKind::Monitor,remaining_ms:216000.0,total_ms:270000.0,channel_counts:BTreeMap::from([("terminal-monitors".into(),2.0)]) }); assert_eq!(label,format!("{}{} goal continues in 3m 36s \u{b7} 2 wake sources on duty","\u{25b0}".repeat(2),"\u{25b1}".repeat(10))); }
    #[test] fn upstream_mixed_channel_singular_and_plural_order() { let counts=BTreeMap::from([("terminal-background-sessions".into(),1.0),("senpi-task".into(),2.0),("terminal-monitors".into(),1.0),("senpi-codemode".into(),2.0)]); assert_eq!(channels_on_duty(&counts),"1 wake source \u{b7} 2 tasks \u{b7} 2 evals \u{b7} 1 bash on duty"); }
    #[test] fn upstream_negative_remaining_is_zero() { let label=format_goal_wait_label(&GoalWaitLabelInput { kind:GoalWaitKind::UserGrace,remaining_ms:-5000.0,total_ms:60000.0,channel_counts:Default::default() }); assert!(label.ends_with("0s")); assert!(!label.contains('-')); }
    #[test] fn upstream_grace_does_not_render_monitor_channels() { let label=format_goal_wait_label(&GoalWaitLabelInput { kind:GoalWaitKind::UserGrace,remaining_ms:30000.0,total_ms:60000.0,channel_counts:BTreeMap::from([("terminal-monitors".into(),3.0)]) }); assert!(!label.contains("monitor")); }
    #[test] fn fractional_and_exponential_counts_match_javascript() { assert_eq!(source_count("senpi-task",1e21),"1e+21 tasks"); assert_eq!(source_count("other",1.5),"1.5 other channels"); }
}
