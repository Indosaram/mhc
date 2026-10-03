pub mod settings;
pub mod ui;
use maho_ext_api::{Extension,ExtensionApi,FlagType,ExtensionContext,NotificationType,ExtensionFailure};
use std::sync::Arc;
pub async fn save_chain(ctx:&ExtensionContext,target:&str,entries:&[String])->Result<bool,ExtensionFailure>{
    let chains=serde_json::json!({target:entries});let warnings=maho_core::retry_fallback::validate::validate_fallback_chains(Some(&chains),&ctx.model_registry.get_all());
    if !warnings.is_empty(){ctx.ui.notify(&warnings.join("\n"),NotificationType::Warning);return Ok(false);}
    ctx.session_settings()?.set_fallback_chain(target,entries).await?;ctx.ui.notify(&format!("Fallback chain saved for {target}."),NotificationType::Info);Ok(true)
}
pub struct ModelFallback{pub is_using_oauth:Arc<dyn Fn(&maho_ext_api::Model)->bool+Send+Sync>}
impl Extension for ModelFallback{
    fn register(&self,api:&mut ExtensionApi){
        api.register_flag("no-model-fallback",FlagType::Boolean{default:Some(false)},Some("Disable retry model fallback for this run.".into()));
        let is_using_oauth=self.is_using_oauth.clone();
        api.register_command("fallback",Some("View and manage retry model fallback chains.".into()),Some("[target [fallback1 fallback2 ...]]".into()),Arc::new(move|args,ctx|{let is_using_oauth=is_using_oauth.clone();Box::pin(async move{
            let args:Vec<&str>=args.split_whitespace().collect();
            if args.is_empty(){return ui::run_fallback_menu(ctx,is_using_oauth.as_ref()).await;}
            if args.len()<2{ctx.ui.notify("Usage: /fallback <target> <fallback1> [fallback2 ...]",NotificationType::Error);return Ok(());}
            save_chain(ctx,args[0],&args[1..].iter().map(|value|(*value).into()).collect::<Vec<_>>()).await?;Ok(())
        })}));
    }
}

