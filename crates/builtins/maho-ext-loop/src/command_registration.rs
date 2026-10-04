use std::sync::Arc;
use maho_ext_api::{ExtensionApi,ExtensionContext,ExtensionFailure,ExtensionMode,NotificationType};
use crate::{command::*,index::{LoopController,LoopCreateOutcome,StartFixedRequest,StartDynamicRequest,StartBareRequest},parse::{parse_loop_args,ParsedLoopInvocation,LoopTarget}};
pub fn register_loop_command(api:&mut ExtensionApi,controller:Arc<dyn LoopController>) {
    api.register_command_with_completions("loop",Some(LOOP_COMMAND_DESCRIPTION.into()),Some(LOOP_ARGUMENT_HINT.into()),Arc::new(move |args,ctx| {
        let controller=Arc::clone(&controller);
        Box::pin(async move {
            if let Err(error)=run_loop_command(args,ctx,controller.as_ref()).await { ctx.ui.notify(&format!("/loop command failed: {}",error.message),NotificationType::Error); }
            Ok(())
        })
    }),Arc::new(|prefix|Box::pin(async move {
        Ok(complete_loop_arguments(prefix).map(|items|items.into_iter().map(|item|maho_tui::autocomplete::AutocompleteItem { value:item.value,label:item.label,description:None }).collect()))
    })));
}
pub async fn run_loop_command(args:&str,ctx:&ExtensionContext,controller:&dyn LoopController)->Result<(),ExtensionFailure> {
    if matches!(ctx.mode,ExtensionMode::Print|ExtensionMode::Json) {
        ctx.ui.notify(LOOP_HEADLESS_REJECTION,NotificationType::Warning); eprintln!("{LOOP_HEADLESS_REJECTION}"); return Ok(());
    }
    let parsed=parse_loop_args(args);
    let (outcome,requested,bare)=match parsed {
        ParsedLoopInvocation::Invalid { usage,.. }=>{ ctx.ui.notify(&usage,NotificationType::Warning); return Ok(()); },
        ParsedLoopInvocation::Status { .. }=>{ let state=controller.get_state(); let status=controller.status_line(); let listing=format_loop_status_listing(status.as_deref(),&state).map_err(|error|ExtensionFailure::new(error.to_string()))?; ctx.ui.notify(&listing,NotificationType::Info); return Ok(()); },
        ParsedLoopInvocation::Stop { target,.. }=>return run_target_command(ctx,controller,&target,"stop").await,
        ParsedLoopInvocation::Pause { target,.. }=>return run_target_command(ctx,controller,&target,"pause").await,
        ParsedLoopInvocation::Resume { target,.. }=>return run_target_command(ctx,controller,&target,"resume").await,
        ParsedLoopInvocation::Fixed { interval,prompt,original_args }=>{ let raw=interval.raw.clone(); (controller.start_fixed(StartFixedRequest { original_args,prompt,requested_interval:interval }).await?,Some(raw),false) },
        ParsedLoopInvocation::Dynamic { prompt,original_args }=>(controller.start_dynamic(StartDynamicRequest { original_args,prompt }).await?,None,false),
        ParsedLoopInvocation::Bare { interval,original_args }=>(controller.start_bare(StartBareRequest { original_args,interval }).await?,None,true),
    };
    match outcome {
        LoopCreateOutcome::Rejected { message }=>ctx.ui.notify(&message,NotificationType::Error),
        LoopCreateOutcome::Created(outcome)=>{
            let message=if bare { let state=controller.get_state(); format_bare_loop_confirmation(&outcome,state.entries.get(&outcome.loop_id)) } else if let Some(raw)=requested { format_fixed_loop_confirmation(&outcome,&raw) } else { format_dynamic_loop_confirmation(&outcome) }.map_err(|error|ExtensionFailure::new(error.to_string()))?;
            ctx.ui.notify(&message,NotificationType::Info);
        },
    }
    Ok(())
}
async fn run_target_command(ctx:&ExtensionContext,controller:&dyn LoopController,target:&LoopTarget,verb:&str)->Result<(),ExtensionFailure> {
    let state=controller.get_state();
    let target=match resolve_command_target(target,&state) {
        TargetResolution::None=>{ ctx.ui.notify("No active loops.",NotificationType::Warning); return Ok(()); },
        TargetResolution::Ambiguous(ids)=>{ ctx.ui.notify(&format!("Multiple loops are active:\n{}\nUse `/loop {verb} <id>` or `/loop {verb} all`.",ids.iter().map(|id|format!("- {id}")).collect::<Vec<_>>().join("\n")),NotificationType::Warning); return Ok(()); },
        TargetResolution::Apply(LoopTarget::All)=>"all".into(),
        TargetResolution::Apply(LoopTarget::Id(id))=>id,
        TargetResolution::Apply(LoopTarget::Implicit)=>return Err(ExtensionFailure::new("Unresolved implicit loop target")),
    };
    let affected=match verb { "stop"=>controller.stop(&target,"user-stop").await?,"pause"=>controller.pause(&target).await?,"resume"=>controller.resume(&target).await?,_=>return Err(ExtensionFailure::new("Unknown loop command")) };
    if affected.is_empty() {
        let message=match (verb,target.as_str()) {
            ("stop","all")=>"No active loops.".into(),("pause","all")=>"No pausable loops.".into(),("resume","all")=>"No paused loops to resume.".into(),
            ("pause",id)=>format!("Loop {id} is not pausable (already paused or ended)."),("resume",id)=>format!("Loop {id} is not paused."),
            (_,id)=>{ let ids=active_loop_ids(&controller.get_state()); let suffix=if ids.is_empty() { String::new() } else { format!("\nActive loops: {}.",ids.join(", ")) }; format!("No active loop with id \"{id}\".{suffix}") },
        };
        ctx.ui.notify(&message,NotificationType::Warning); return Ok(());
    }
    let past=match verb { "stop"=>"Stopped","pause"=>"Paused","resume"=>"Resumed",_=>return Err(ExtensionFailure::new("Unknown loop command")) };
    let message=if target=="all" { format!("{past} {} {}: {}.",affected.len(),if affected.len()==1 { "loop" } else { "loops" },affected.join(", ")) } else { format!("{past} loop {target}.") };
    ctx.ui.notify(&message,NotificationType::Info); Ok(())
}
