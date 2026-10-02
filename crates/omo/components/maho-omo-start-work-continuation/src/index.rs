use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use crate::boulder_eligibility::{find_continuable_boulder_work,ContinuableWork};
pub const START_WORK_STEERING_REMINDER:&str="<omo-senpi-start-work>\nAn active Prometheus start-work plan is present in this working directory.\nBefore continuing, read `.omo/boulder.json` and the active plan file to determine what remains; use the ledger and plan as the source of truth.\nContinue the current work with evidence-bound execution; do not start unrelated work until every top-level checkbox is `- [x]`.\n</omo-senpi-start-work>";
#[derive(Default)]
struct State { consecutive:usize,last_signature:Option<String>,same_signature_retries:usize }
pub struct StartWorkContinuationComponent { pub continuation_limit:usize,pub max_same_signature_retries:usize }
impl Default for StartWorkContinuationComponent { fn default()->Self { Self{continuation_limit:8,max_same_signature_retries:1} } }
impl Extension for StartWorkContinuationComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let state=Arc::new(Mutex::new(State::default()));let input_state=Arc::clone(&state);
        api.on(EventKind::Input,Arc::new(move |event,ctx| { let state=Arc::clone(&input_state);Box::pin(async move {
            let ExtensionEvent::Input(input)=event else { return Ok(EventResult::None); };
            if input.source==InputSource::Extension { return Ok(EventResult::Input(InputEventResult::Continue)); }
            *state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=State::default();
            if input.streaming_behavior.is_some() && !ctx.session_manager.session_id().is_empty() && find_continuable_boulder_work(&ctx.cwd,ctx.session_manager.session_id()).map_err(|e|ExtensionFailure::new(e.to_string()))?.is_some() {
                return Ok(EventResult::Input(InputEventResult::Transform{text:format!("{}\n\n{START_WORK_STEERING_REMINDER}",input.text),images:input.images.clone()}));
            }
            Ok(EventResult::Input(InputEventResult::Continue))
        }) }));
        let limit=self.continuation_limit;let retries=self.max_same_signature_retries;let runtime=api.runtime.clone();
        api.on(EventKind::AgentEnd,Arc::new(move |_,ctx| { let state=Arc::clone(&state);let runtime=runtime.clone();Box::pin(async move {
            let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.consecutive>=limit || ctx.session_manager.session_id().is_empty() { return Ok(EventResult::None); }
            let Some(work)=find_continuable_boulder_work(&ctx.cwd,ctx.session_manager.session_id()).map_err(|e|ExtensionFailure::new(e.to_string()))? else { state.last_signature=None;state.same_signature_retries=0;return Ok(EventResult::None); };
            let signature=format!("{}:{}:{}/{}",work.work.work_id().unwrap_or_default(),work.work.updated_at().or_else(||work.work.started_at()).unwrap_or_default(),work.checklist.completed,work.checklist.total);
            if state.last_signature.as_ref()==Some(&signature) { if state.same_signature_retries>=retries { return Ok(EventResult::None); } state.same_signature_retries+=1; }
            else { state.last_signature=Some(signature);state.same_signature_retries=0; }
            state.consecutive+=1;
            let content=render_directive(&work,&ctx.cwd,ctx.session_manager.session_id());
            let api=ExtensionApi::new(LoadedExtension::new("start-work-continuation",ctx.cwd.clone(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
            api.send_message(CustomMessage{custom_type:"omo-senpi:start-work-continuation".into(),content:vec![ToolContent::text(content)],display:false,details:None},SendMessageOptions{trigger_turn:true,deliver_as:Some(DeliverAs::FollowUp)})?;
            Ok(EventResult::None)
        }) }));
    }
}
pub fn render_directive(work:&ContinuableWork,cwd:&std::path::Path,session_id:&str)->String {
    let worktree=work.work.worktree_path().map(|p|format!("\n- Worktree: `{p}` (all edits, tests, and commands run inside this directory)")).unwrap_or_default();
    let final_hint=if work.checklist.remaining==0 { "\nAll top-level checkboxes are complete. Run the Final Verification Wave and mark the boulder work completed." } else { "" };
    vec![
        "<omo-senpi-start-work-continuation>".into(),
        "You are mid-flight on a Prometheus work plan; this turn is an automatic continuation. Do NOT ask whether to continue — the contract is auto-continue until every top-level checkbox is `- [x]`.".into(),"".into(),"# State".into(),"".into(),
        format!("- Plan: `{}`",work.work.plan_name().unwrap_or_default()),format!("- Plan file: `{}`",work.plan_path.display()),format!("- Boulder state: `{}`",cwd.join(".omo/boulder.json").display()),
        format!("- Remaining top-level checkboxes: {} of {}",work.checklist.remaining,work.checklist.total),format!("- [Status: {}/{}, next: {}]",work.checklist.completed,work.checklist.total,work.checklist.next_task_label.as_deref().unwrap_or("none (final gate pending)")),format!("- Ledger: `{}`",cwd.join(".omo/start-work/ledger.jsonl").display()),format!("- Your session id in boulder.json: senpi:{session_id}"),worktree,"".into(),"# What to do this turn".into(),"".into(),
        "1. Read the plan file AND the ledger first — they are the only sources of truth for what remains and what evidence exists; do not trust your memory of prior turns.".into(),
        format!("2. When the remaining count is `0`, skip checkbox execution and perform the Final gate now. Otherwise, pick the FIRST unchecked top-level checkbox in `## TODOs` or `## Final Verification Wave`.{final_hint}"),
        "3. Apply the checkbox's tier and verify with real-surface evidence. Decompose and dispatch sub-tasks in parallel via Senpi's `task` tool when safe.".into(),
        "4. Honor the delivery mode recorded in the goal/ledger at session start: `--make-pr` finishes through the task-owned worktree and an opened PR, then hands off with the PR URL; `--ship` keeps working until that PR is MERGED, then removes the worktree and syncs `.omo/` state back.".into(),
        "5. After verification, apply the checkbox, append a durable evidence record to the ledger, and continue.".into(),"</omo-senpi-start-work-continuation>".into(),
    ].join("\n")
}
