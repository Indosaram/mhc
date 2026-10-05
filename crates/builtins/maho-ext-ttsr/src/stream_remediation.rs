use maho_ai::types::AssistantMessage;
use crate::{prompts::COLLAPSE_RULE_CONTENT,remediation::{build_error_shell_replacement,build_nudge_message,build_truncate_replacement,ErrorShellReplacement,TtsrNudgeMessage},types::*};
pub struct StreamRemediationInput { pub resolution:DetectionResolution,pub stream_kind:TtsrStreamSource }
pub enum StreamReplacement { ErrorShell(ErrorShellReplacement),Truncated(Box<AssistantMessage>) }
pub struct StreamRemediationOutcome { pub replacement:StreamReplacement,pub nudge:Option<TtsrNudgeMessage>,pub owner:DetectionOwner,pub observed_rules:Vec<DetectionOwner>,pub retry_mode:&'static str }
fn owner_name(owner:DetectionOwner)->&'static str { match owner { DetectionOwner::CollapseRepetition=>"collapse-repetition",DetectionOwner::ControlTokenLeak=>"control-token-leak" } }
pub fn build_stream_remediation(pending:StreamRemediationInput,message:AssistantMessage)->StreamRemediationOutcome {
    let resolution=pending.resolution;
    if resolution.remediation.corruption_scope()=="generation" { return StreamRemediationOutcome { replacement:StreamReplacement::ErrorShell(build_error_shell_replacement()),nudge:None,owner:resolution.owner,observed_rules:resolution.observed_rules,retry_mode:"provider-error" }; }
    StreamRemediationOutcome { replacement:StreamReplacement::Truncated(Box::new(build_truncate_replacement(message,resolution.detection.garbage_start_offset,pending.stream_kind))),nudge:Some(build_nudge_message(owner_name(resolution.owner),COLLAPSE_RULE_CONTENT)),owner:resolution.owner,observed_rules:resolution.observed_rules,retry_mode:"nudge" }
}
#[cfg(test)] mod tests {
    use super::*;
    fn message()->AssistantMessage { serde_json::from_value(serde_json::json!({"content":[{"type":"text","text":"abcgarbage"}],"api":"faux","provider":"faux","model":"fixture","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"aborted","timestamp":0})).unwrap() }
    fn resolution(owner:DetectionOwner)->DetectionResolution { DetectionResolution { owner,observed_rules:vec![owner],detection:DetectorMatch { rule:DetectorRule::CollapseRepetition,reason:String::new(),anomaly_start_offset:3,garbage_start_offset:3,detail:Default::default() },remediation:if owner==DetectionOwner::ControlTokenLeak { CONTROL_LEAK_REMEDIATION } else { collapse_remediation() } } }
    #[test] fn generation_corruption_returns_retry_shell_without_nudge() { let result=build_stream_remediation(StreamRemediationInput { resolution:resolution(DetectionOwner::ControlTokenLeak),stream_kind:TtsrStreamSource::Text },message()); assert!(matches!(result.replacement,StreamReplacement::ErrorShell(_))); assert!(result.nudge.is_none()); assert_eq!(result.retry_mode,"provider-error"); }
    #[test] fn output_corruption_truncates_and_attributes_hidden_nudge() { let result=build_stream_remediation(StreamRemediationInput { resolution:resolution(DetectionOwner::CollapseRepetition),stream_kind:TtsrStreamSource::Text },message()); let StreamReplacement::Truncated(message)=result.replacement else { panic!("expected truncation"); }; assert!(matches!(&message.content[0],maho_ai::types::ContentBlock::Text(text) if text.text=="abc")); assert_eq!(result.nudge.unwrap().details.rules,["collapse-repetition"]); assert_eq!(result.retry_mode,"nudge"); }
}
