use maho_ext_api::{ExtensionContext,ExtensionFailure,NotificationType,ExtensionUiDialogOptions,FallbackRevertPolicy,RetryFallbackSettings};
use crate::{settings::load_fallback_settings,save_chain};
pub fn render_fallback_state(settings:&RetryFallbackSettings,status:Option<maho_ext_api::RetryFallbackStatus>)->String{
    let rows=settings.chains.iter().map(|(target,entries)|format!("{target} -> {}",entries.join(", "))).collect::<Vec<_>>();
    let chains=if rows.is_empty(){"No fallback chains configured.".into()}else{rows.join("\n")};
    let live=match status{Some(status) if status.active=>format!("active on {} from {}{}",status.current_model.as_deref().unwrap_or("unknown"),status.original_selector.as_deref().unwrap_or("unknown"),if status.pinned{" (pinned)"}else{""}),_=>"inactive".into()};
    format!("{chains}\nModel fallback: {}\nRevert policy: {}\nLive retry state: {live}",if settings.model_fallback{"enabled"}else{"disabled"},if settings.revert_policy==FallbackRevertPolicy::Never{"never"}else{"cooldown-expiry"})
}
async fn select(ctx:&ExtensionContext,title:&str,items:Vec<String>)->Option<String>{ctx.ui.select(title,&items,ExtensionUiDialogOptions::default()).await}
pub async fn run_fallback_menu(ctx:&ExtensionContext)->Result<(),ExtensionFailure>{
    if !ctx.has_ui{ctx.ui.notify("Fallback menu requires interactive UI. Use /fallback <target> <fallback...>.",NotificationType::Error);return Ok(());}
    let session=ctx.session_settings()?;let settings=load_fallback_settings(session,ctx.model_registry.as_ref());
    let Some(choice)=select(ctx,"Model fallback",["Show chains & live state","Add/edit chain","Remove chain","Toggle model fallback","Revert policy"].map(str::to_owned).to_vec()).await else{return Ok(())};
    match choice.as_str(){
        "Show chains & live state"=>ctx.ui.notify(&render_fallback_state(&settings,session.get_fallback_status()),NotificationType::Info),
        "Add/edit chain"=>{
            let models=ctx.model_registry.get_available();let names:Vec<String>=models.iter().map(|model|format!("{}/{}",model.provider,model.id)).collect();
            let Some(target)=select(ctx,"Fallback target model",names.clone()).await else{return Ok(())};let mut entries=Vec::new();
            loop{
                let Some(fallback)=select(ctx,"Fallback model (Done to save)",[vec!["Done".into()],names.clone()].concat()).await else{break};if fallback=="Done"{break;}
                let Some(model)=models.iter().find(|model|format!("{}/{}",model.provider,model.id)==fallback)else{return Ok(())};
                let levels=maho_core::thinking_levels::get_supported_thinking_levels(model).iter().map(|level|level.as_str().to_owned()).collect::<Vec<_>>();
                let Some(thinking)=select(ctx,"Thinking level",[vec!["inherit".into()],levels].concat()).await else{return Ok(())};entries.push(if thinking=="inherit"{fallback}else{format!("{fallback}:{thinking}")});
            }
            if !entries.is_empty(){save_chain(ctx,&target,&entries).await?;}
        }
        "Remove chain"=>{if let Some(target)=select(ctx,"Remove fallback chain",settings.chains.keys().cloned().collect()).await{session.remove_fallback_chain(&target).await?;ctx.ui.notify(&format!("Removed fallback chain for {target}."),NotificationType::Info);}}
        "Toggle model fallback"=>{session.set_model_fallback_enabled(!settings.model_fallback).await?;ctx.ui.notify(&format!("Model fallback {}.",if settings.model_fallback{"disabled"}else{"enabled"}),NotificationType::Info);}
        _=>{if let Some(policy)=select(ctx,"Fallback revert policy",vec!["cooldown-expiry".into(),"never".into()]).await{let policy=match policy.as_str(){"cooldown-expiry"=>FallbackRevertPolicy::CooldownExpiry,"never"=>FallbackRevertPolicy::Never,_=>return Ok(())};session.set_fallback_revert_policy(policy).await?;}}
    }
    Ok(())
}
