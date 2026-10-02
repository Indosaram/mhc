use crate::{detectors::LoopGuardDetection,notice::LoopGuardEscalationDetails};
use maho_ext_host::notice::{adapters::notice_message_renderer,spec::{NoticeSpec,NoticeTone}};
pub fn render_loop_guard_notice()->maho_ext_api::MessageRenderer {
    notice_message_renderer(|message| {
        let details=message.details.as_ref()?;
        let count=usize::try_from(details["count"].as_u64()?).ok()?;
        let fingerprint=details["fingerprint"].as_str()?.to_owned();
        let detection=match details["kind"].as_str()? {
            "identical"=>LoopGuardDetection::Identical { tool_name:details["toolName"].as_str()?.into(),count,fingerprint },
            "similar"=>LoopGuardDetection::Similar { tool_name:details["toolName"].as_str()?.into(),count,similarity:details["similarity"].as_f64()?,fingerprint },
            "cycle"=>LoopGuardDetection::Cycle { period:usize::try_from(details["period"].as_u64()?).ok()?,count,cycle_tools:details["cycleTools"].as_array()?.iter().map(|value|value.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?,fingerprint },
            _=>return None,
        };
        Some(NoticeSpec { title:title_line(&detection),tone:None,why:why_line(&detection),extra:Vec::new(),expanded_line:Some(expanded_line(&detection)) })
    })
}
pub fn render_loop_guard_escalation()->maho_ext_api::MessageRenderer {
    notice_message_renderer(|message| {
        let data=message.details.as_ref()?;
        let details=LoopGuardEscalationDetails { tool_name:data["toolName"].as_str()?.into(),blocked_call_count:usize::try_from(data["blockedCallCount"].as_u64()?).ok()? };
        Some(NoticeSpec { title:escalation_title(&details),tone:Some(NoticeTone::Error),why:escalation_why(&details),extra:Vec::new(),expanded_line:Some(ESCALATION_EXPANDED_LINE.into()) })
    })
}
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
    #[test] fn native_notice_adapter_handles_absent_details_and_expansion() {
        let renderer=render_loop_guard_notice();
        let mut message=maho_ext_api::CustomMessage { custom_type:crate::notice::LOOP_GUARD_NOTICE_CUSTOM_TYPE.into(),content:Vec::new(),display:true,details:None };
        let theme=maho_ext_api::Theme::default();
        assert!(renderer(&message,&Default::default(),&theme).is_none());
        message.details=Some(serde_json::json!({"kind":"identical","toolName":"read","count":4,"fingerprint":"f"}));
        let mut collapsed=renderer(&message,&Default::default(),&theme).unwrap();
        let mut expanded=renderer(&message,&maho_ext_api::MessageRenderOptions { expanded:true,..Default::default() },&theme).unwrap();
        assert!(expanded.render(80).len()>collapsed.render(80).len());
    }
    #[test] fn similarity_percentage_uses_javascript_halfway_and_nonfinite_numbers() {
        for (value,expected) in [(0.125,"13"),(-0.125,"-12"),(f64::NAN,"NaN"),(f64::INFINITY,"Infinity")] { assert_eq!(percent(value),expected); }
    }
}
