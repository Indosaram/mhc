use std::{path::PathBuf, sync::{Arc, Mutex}};
use maho_ext_api::*;
use crate::state::claim_onboarding;

pub struct OnboardingComponent { pub state_dir: PathBuf, pub skills_root: String }
impl Extension for OnboardingComponent {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag("onboard",FlagType::Boolean{default:Some(false)},Some("Force the onboarding flow on startup.".into()));
        let runtime=api.runtime.clone(); let state_dir=self.state_dir.clone(); let skills_root=self.skills_root.clone();
        let consumed=Arc::new(Mutex::new(false));
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {
            let has_ui=ctx.has_ui;
            let runtime=runtime.clone(); let state_dir=state_dir.clone(); let skills_root=skills_root.clone(); let consumed=Arc::clone(&consumed);
            Box::pin(async move {
                let ExtensionEvent::SessionStart(event)=event else { return Ok(EventResult::None); };
                if event.reason != SessionReason::Startup || !has_ui || runtime.get_flag("omo-senpi-onboarding-disabled")==Some(FlagValue::Boolean(true)) { return Ok(EventResult::None); }
                let force=runtime.get_flag("onboard")==Some(FlagValue::Boolean(true));
                if force && *consumed.lock().unwrap_or_else(std::sync::PoisonError::into_inner) { return Ok(EventResult::None); }
                if !force && !claim_onboarding(&state_dir) { return Ok(EventResult::None); }
                let api=ExtensionApi::new(LoadedExtension::new("onboarding",PathBuf::new(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
                api.send_message(CustomMessage{custom_type:"omo-onboarding:bootstrap".into(),content:vec![ToolContent::text(format!("Read the onboarding skill at {skills_root}/onboarding/SKILL.md with the read tool and follow it. Greet the user first."))],display:false,details:None},SendMessageOptions{trigger_turn:true,deliver_as:Some(DeliverAs::FollowUp)})?;
                if force { *consumed.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=true; }
                api.append_entry("omo-onboarding:started",Some(serde_json::json!({"reason":"startup","forced":force})))?;
                Ok(EventResult::None)
            })
        }));
    }
}
