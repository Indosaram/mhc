use std::{sync::{Arc,Mutex},time::{SystemTime,UNIX_EPOCH}};
use maho_ext_api::{AbortSource,AgentMessage,CustomMessage,EventKind,EventResult,Extension,ExtensionApi,ExtensionContext,ExtensionEvent,ExtensionFailure,FlagType,FlagValue,SendMessageOptions,ToolContent};
use crate::{builtin_rules::builtin_ttsr_rules,commands::{register_ttsr_commands,TtsrPublicState},coordinator::{claim_abort,mark_user_cancelled},discovery::discover_ttsr_rules_sync,manager::TtsrManager,message_update::get_ttsr_stream_delta,prompts::REPETITIVE_TURNS_RULE_CONTENT,remediation::{build_nudge_message,TtsrNudgeMessage},repetitive_turns_lane::{RepetitiveTurnsLane,read_persisted_assistant_texts},stream_remediation::{build_stream_remediation,StreamRemediationInput,StreamReplacement},types::*,watch::StreamWatcher};
use crate::detectors::repetitive_turns::REPETITIVE_TURNS_RULE_NAME;
#[derive(Default)]
struct State { watcher:Option<StreamWatcher>,generation_state:GenerationDetectionState,generation:u64,pending_remediation:Option<StreamRemediationInput>,pending_rule:Option<TtsrRule>,pending_nudge:Option<TtsrNudgeMessage>,settling_user_abort:bool,disabled:bool,repetitive_turns:RepetitiveTurnsLane }
fn now()->u64 { SystemTime::now().duration_since(UNIX_EPOCH).map_or(0,|duration|duration.as_millis().min(u128::from(u64::MAX)) as u64) }
fn owner_name(owner:DetectionOwner)->&'static str { match owner { DetectionOwner::CollapseRepetition=>"collapse-repetition",DetectionOwner::ControlTokenLeak=>"control-token-leak" } }
fn record_injection(api:&ExtensionApi,owner:&str,rules:&[String],mode:&str)->Result<(),ExtensionFailure> { api.append_entry("rule-activation",Some(serde_json::json!({"kind":"ttsr","owner":owner,"rules":rules,"remediation":mode}))) }
impl State {
    fn cancel(&mut self) { if self.pending_remediation.is_some() || self.pending_nudge.is_some() || self.pending_rule.is_some() { mark_user_cancelled(&mut self.generation_state); self.pending_remediation=None; self.pending_rule=None; self.pending_nudge=None; self.repetitive_turns.disarm(); } }
    fn reset_generation(&mut self) { self.generation+=1; self.generation_state=GenerationDetectionState::default(); self.pending_remediation=None; self.pending_rule=None; self.repetitive_turns.reset_turn(); if let Some(watcher)=&mut self.watcher { watcher.reset(); } }
    fn initialize(&mut self,api:&ExtensionApi,ctx:&ExtensionContext) {
        if self.watcher.is_some() { return; }
        self.disabled=matches!(api.get_flag("ttsr-disabled"),Some(FlagValue::Boolean(true)));
        let disabled=match api.get_flag("ttsr-rules-disabled") { Some(FlagValue::String(value))=>value.split(',').map(str::trim).filter(|name|!name.is_empty()).map(str::to_owned).collect::<Vec<_>>(),_=>vec![] };
        self.repetitive_turns.configure(&disabled.iter().cloned().collect());
        let mut manager=TtsrManager::new(TtsrSettings { enabled:!self.disabled,disabled_rules:disabled.clone(),..Default::default() });
        let entries=ctx.session_manager.get_entries();
        for entry in &entries {
            let value=&entry.data; if value["type"]!="custom" { continue; }
            let data=&value["data"];
            let legacy=value["customType"]==TTSR_INJECTION_CUSTOM_TYPE;
            let activation=value["customType"]=="rule-activation" && data["kind"]=="ttsr" && data["owner"].as_str().is_some_and(|owner|!owner.is_empty()) && matches!(data["remediation"].as_str(),Some("nudge"|"provider-error")) && data["rules"].as_array().is_some_and(|rules|!rules.is_empty() && rules.iter().all(|rule|rule.as_str().is_some_and(|rule|!rule.is_empty())));
            if (legacy || activation) && let Some(rules)=data["rules"].as_array() { manager.restore_injected(&rules.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect::<Vec<_>>()); }
        }
        for rule in builtin_ttsr_rules() { manager.add_rule(rule); }
        if let Some(home)=std::env::var_os("HOME") { for rule in discover_ttsr_rules_sync(&ctx.cwd,&std::path::PathBuf::from(home)).rules { manager.add_rule(rule); } }
        self.repetitive_turns.restore_from_history(&read_persisted_assistant_texts(&entries.into_iter().map(|entry|entry.data).collect::<Vec<_>>()));
        self.watcher=Some(StreamWatcher::new(manager,&disabled));
    }
    fn public_state(&self)->TtsrPublicState { TtsrPublicState { rules:self.watcher.as_ref().map_or_else(Vec::new,|watcher|watcher.manager.rules()),injected_rule_names:self.watcher.as_ref().map_or_else(Vec::new,|watcher|watcher.manager.injected_rule_names()),disabled:self.disabled } }
}
fn handle(state:&mut State,api:&ExtensionApi,event:&ExtensionEvent,ctx:&ExtensionContext)->Result<EventResult,ExtensionFailure> {
    match event {
        ExtensionEvent::SessionStart(_)=>state.initialize(api,ctx),
        ExtensionEvent::SessionAbort|ExtensionEvent::Input(_)=>state.cancel(),
        ExtensionEvent::AgentEnd { abort_source,will_retry,.. }=>{ state.settling_user_abort=*abort_source==Some(AbortSource::User); if state.settling_user_abort { state.cancel(); } else if *will_retry==Some(true) { state.reset_generation(); } },
        ExtensionEvent::TurnStart { .. }=>{ state.initialize(api,ctx); state.reset_generation(); },
        ExtensionEvent::TurnEnd { .. }=>{ if let Some(watcher)=&mut state.watcher { watcher.manager.increment_message_count(); } },
        ExtensionEvent::MessageUpdate { assistant_message_event,.. }=>{
            state.initialize(api,ctx); if state.disabled { return Ok(EventResult::None); }
            let event:maho_ai::types::AssistantMessageEvent=serde_json::from_value(assistant_message_event.clone()).map_err(|error|ExtensionFailure::new(error.to_string()))?;
            let Some(delta)=get_ttsr_stream_delta(&event) else { return Ok(EventResult::None); };
            let Some(watcher)=&mut state.watcher else { return Ok(EventResult::None); };
            let outcome=watcher.handle_delta(delta.source,&delta.stream_key,delta.delta,state.generation,delta.tool_name);
            if let Some(resolution)=outcome.resolution && claim_abort(&mut state.generation_state,&resolution,now()) { state.pending_remediation=Some(StreamRemediationInput { resolution,stream_kind:delta.source }); ctx.abort(Some(AbortSource::System))?; return Ok(EventResult::None); }
            if delta.source==TtsrStreamSource::Text && state.repetitive_turns.observe_text_delta(delta.delta,state.pending_rule.is_none() && !state.generation_state.abort_claimed) && !state.generation_state.abort_claimed { state.generation_state.abort_claimed=true; state.generation_state.abort_owner=Some(DetectionOwner::CollapseRepetition); state.generation_state.self_abort_at=Some(now()); ctx.abort(Some(AbortSource::System))?; return Ok(EventResult::None); }
            if state.pending_rule.is_none() && !state.generation_state.abort_claimed && let Some(rule)=outcome.rule_matches.into_iter().find(|rule|rule.interrupt_mode==TtsrInterruptMode::Always) { state.generation_state.abort_claimed=true; state.generation_state.abort_owner=Some(DetectionOwner::CollapseRepetition); state.generation_state.self_abort_at=Some(now()); state.pending_rule=Some(rule); ctx.abort(Some(AbortSource::System))?; }
        },
        ExtensionEvent::MessageEnd { message }=>{
            if let Some(watcher)=&mut state.watcher { watcher.manager.reset_buffers(); }
            if state.generation_state.user_cancelled { return Ok(EventResult::None); }
            let Some(assistant)=message.as_assistant() else { return Ok(EventResult::None); };
            let text=assistant.content.iter().filter_map(|block|match block { maho_ai::types::ContentBlock::Text(text)=>Some(text.text.as_str()),_=>None }).collect::<String>(); let text=(!text.is_empty()).then_some(text);
            if state.repetitive_turns.armed() {
                state.repetitive_turns.commit_armed_turn(text.as_deref()); let names=vec![REPETITIVE_TURNS_RULE_NAME.into()]; if let Some(watcher)=&mut state.watcher { watcher.manager.mark_injected_by_names(&names); } record_injection(api,REPETITIVE_TURNS_RULE_NAME,&names,"nudge")?; state.pending_nudge=Some(build_nudge_message(REPETITIVE_TURNS_RULE_NAME,REPETITIVE_TURNS_RULE_CONTENT));
            } else if let Some(rule)=state.pending_rule.take() {
                let names=vec![rule.name.clone()]; if let Some(watcher)=&mut state.watcher { watcher.manager.mark_injected_by_names(&names); } record_injection(api,&rule.name,&names,"nudge")?; state.pending_nudge=Some(build_nudge_message(&rule.name,&rule.content));
            } else if let Some(pending)=state.pending_remediation.take() {
                if let Some(text)=&text { state.repetitive_turns.record_completed_turn(text); }
                let outcome=build_stream_remediation(pending,assistant.clone()); record_injection(api,owner_name(outcome.owner),&outcome.observed_rules.into_iter().map(|owner|owner_name(owner).into()).collect::<Vec<_>>(),outcome.retry_mode)?;
                if outcome.nudge.is_some() { state.pending_nudge=outcome.nudge; }
                let replacement=match outcome.replacement { StreamReplacement::Truncated(message)=>*message,StreamReplacement::ErrorShell(shell)=>{ let mut message=assistant.clone(); message.content=shell.content; message.stop_reason=shell.stop_reason; message.error_message=Some(shell.error_message); message } };
                return Ok(EventResult::MessageEnd { message:Some(AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(replacement)))) });
            } else if let Some(text)=text { state.repetitive_turns.record_completed_turn(&text); }
        },
        ExtensionEvent::AgentSettled=>{
            let nudge=state.pending_nudge.take(); if state.generation_state.user_cancelled || state.settling_user_abort { state.settling_user_abort=false; return Ok(EventResult::None); }
            if let Some(nudge)=nudge { api.send_message(CustomMessage { custom_type:nudge.custom_type,content:vec![ToolContent::text(nudge.content)],display:nudge.display,details:Some(serde_json::json!({"rules":nudge.details.rules})) },SendMessageOptions { trigger_turn:true,deliver_as:None })?; }
        },
        _=>{},
    }
    Ok(EventResult::None)
}
pub struct TtsrExtension;
impl Extension for TtsrExtension {
    fn register(&self,api:&mut ExtensionApi) {
        api.register_flag("ttsr-disabled",FlagType::Boolean { default:Some(false) },Some("Disable TTSR stream-rule detection.".into()));
        api.register_flag("ttsr-rules-disabled",FlagType::String { default:Some(String::new()) },Some("Comma-separated TTSR rule names to disable.".into()));
        let state=Arc::new(Mutex::new(State::default())); let public=Arc::clone(&state); register_ttsr_commands(api,Arc::new(move ||public.lock().unwrap_or_else(std::sync::PoisonError::into_inner).public_state()));
        let actions=Arc::new(ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        for kind in [EventKind::SessionStart,EventKind::SessionAbort,EventKind::Input,EventKind::AgentEnd,EventKind::TurnStart,EventKind::TurnEnd,EventKind::MessageUpdate,EventKind::MessageEnd,EventKind::AgentSettled] { let state=Arc::clone(&state); let actions=Arc::clone(&actions); api.on(kind,Arc::new(move |event,ctx| { let state=Arc::clone(&state); let actions=Arc::clone(&actions); Box::pin(async move { let mut state=state.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?; handle(&mut state,&actions,event,ctx) }) })); }
    }
}
#[cfg(test)] mod tests {
    use super::*;
    #[test] fn native_registration_exposes_flags_command_and_nine_hooks() { let mut api=ExtensionApi::new(maho_ext_api::LoadedExtension::new("ttsr",".".into(),Default::default()),Default::default(),Default::default(),Default::default()); TtsrExtension.register(&mut api); assert_eq!(api.registered.handlers.len(),9); assert_eq!(api.registered.commands[0].name,"ttsr"); assert_eq!(api.registered.flags.len(),2); }
    #[test] fn cancel_drops_remediation_and_disarms_recovery() { let mut state=State { pending_nudge:Some(build_nudge_message("test","test")),..Default::default() }; state.cancel(); assert!(state.generation_state.user_cancelled); assert!(state.pending_nudge.is_none()); }
    #[test] fn generation_reset_preserves_queued_nudge_but_releases_abort_claim() { let mut state=State { pending_nudge:Some(build_nudge_message("test","test")),generation_state:GenerationDetectionState { abort_claimed:true,..Default::default() },..Default::default() }; state.reset_generation(); assert!(!state.generation_state.abort_claimed); assert!(state.pending_nudge.is_some()); assert_eq!(state.generation,1); }
}
