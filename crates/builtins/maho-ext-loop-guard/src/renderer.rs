use crate::{detectors::LoopGuardDetection,notice::LoopGuardEscalationDetails};
pub fn title_line(detection:&LoopGuardDetection)->String {
    match detection {
        LoopGuardDetection::Identical { tool_name,count,.. }=>format!("⚠ Loop guard · identical calls ×{count} ({tool_name})"),
        LoopGuardDetection::Similar { tool_name,count,.. }=>format!("⚠ Loop guard · near-identical calls ×{count} ({tool_name})"),
        LoopGuardDetection::Cycle { period,count,.. }=>format!("⚠ Loop guard · repeating pattern ×{count} (period {period})"),
    }
}
fn percent(similarity:f64)->String { maho_ai::utils::js::number_to_string((similarity*100.0+0.5).floor()) }
pub fn why_line(detection:&LoopGuardDetection)->String {
    match detection {
        LoopGuardDetection::Identical { .. }=>"Same tool, same arguments, again. The agent was told to reuse the result or change the call.".into(),
        LoopGuardDetection::Similar { similarity,.. }=>format!("Argument similarity ~{}%. The agent was told to verify this is distinct work, not a lazy loop.",percent(*similarity)),
        LoopGuardDetection::Cycle { cycle_tools,.. }=>format!("Tool-call cycle [{}]. The agent was told to break the rotation or justify the progress.",cycle_tools.join(" -> ")),
    }
}
pub fn expanded_line(detection:&LoopGuardDetection)->String {
    match detection {
        LoopGuardDetection::Identical { tool_name,count,.. }=>format!("tool {tool_name} · {count} consecutive identical calls when the reminder fired"),
        LoopGuardDetection::Similar { tool_name,count,similarity,.. }=>format!("tool {tool_name} · {count} consecutive same-tool calls at ~{}% args similarity",percent(*similarity)),
        LoopGuardDetection::Cycle { cycle_tools,count,.. }=>format!("cycle {} · {count} full repetitions in the tracked window",cycle_tools.join(" -> ")),
    }
}
pub fn escalation_title(details:&LoopGuardEscalationDetails)->String { format!("! Loop guard · turn interrupted ({})",details.tool_name) }
pub fn escalation_why(details:&LoopGuardEscalationDetails)->String { format!("The agent ignored two warnings and repeated {} calls after blocking began.",details.blocked_call_count) }
pub const ESCALATION_EXPANDED_LINE:&str="A user-role recovery message was queued and the active turn was stopped by a system abort.";
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn similarity_percentage_uses_javascript_halfway_and_nonfinite_numbers() {
        for (value,expected) in [(0.125,"13"),(-0.125,"-12"),(f64::NAN,"NaN"),(f64::INFINITY,"Infinity")] { assert_eq!(percent(value),expected); }
    }
}
