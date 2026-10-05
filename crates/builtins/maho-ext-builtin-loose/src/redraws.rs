use maho_ext_api::*;
use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
pub type FullRedrawCounter=Arc<dyn Fn(&dyn ExtensionTuiHost)->u64+Send+Sync>;
pub struct Redraws{pub full_redraws:FullRedrawCounter}
impl Extension for Redraws{
    fn register(&self,api:&mut ExtensionApi){
        let full_redraws=self.full_redraws.clone();
        api.register_command("tui",Some("Show TUI stats".into()),None,Arc::new(move|_,ctx|{let full_redraws=full_redraws.clone();Box::pin(async move{
            if !ctx.has_ui{return Ok(());}
            let count=Arc::new(AtomicU64::new(0));let captured=count.clone();
            ctx.ui.custom_factory(Arc::new(move|host,_,_,done|{
                captured.store(full_redraws(host),Ordering::SeqCst);done(JsonValue::Null);
                Box::pin(async{Ok(Box::new(maho_tui::components::text::Text::with_padding("",0,0)) as Box<dyn Component>)})
            }),Default::default()).await?;
            ctx.ui.notify(&format!("TUI full redraws: {}",count.load(Ordering::SeqCst)),NotificationType::Info);Ok(())
        })}));
    }
}
