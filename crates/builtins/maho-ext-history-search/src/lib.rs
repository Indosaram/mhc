pub mod types;
pub mod filter;
pub mod indexer;
pub mod overlay;

use std::path::{Path,PathBuf};
pub fn resolve_search_root(current:&Path,default:&Path)->PathBuf {
    let normalize=|path:&Path|{
        let absolute=if path.is_absolute(){path.to_owned()}else{std::env::current_dir().expect("working directory available").join(path)};
        let mut result=PathBuf::new();for component in absolute.components(){match component{std::path::Component::CurDir=>{},std::path::Component::ParentDir=>{result.pop();},component=>result.push(component.as_os_str())}}result
    };
    let default=normalize(default);
    if current.as_os_str().is_empty(){return default;}
    let current=normalize(current);
    if current.starts_with(&default){default}else{current}
}
pub fn resolve_search_root_windows(current:&str,default:&str)->String{
    let normalize=|input:&str|{let input=input.replace('/',"\\");let mut parts=Vec::new();for part in input.split('\\'){match part{""|"."=>{},".."=>{if parts.len()>1{parts.pop();}},part=>parts.push(part)}}parts.join("\\")};
    let default=normalize(default);if current.is_empty(){return default;}let current=normalize(current);
    if current.eq_ignore_ascii_case(&default)||current.to_lowercase().starts_with(&format!("{}\\",default.to_lowercase())){default}else{current}
}

use maho_ext_api::*;
use std::sync::Arc;
pub type HistorySelection=Arc<dyn Fn(ExtensionContext,Vec<types::HistoryEntry>)->ExtensionFuture<'static,Option<types::HistoryEntry>>+Send+Sync>;
pub type HistoryRenderer=Arc<dyn Fn(&dyn ExtensionTuiHost)->std::rc::Rc<dyn Fn()>+Send+Sync>;
pub type HistoryThemeMapper=Arc<dyn Fn(&Theme)->Result<maho_interactive::theme::Theme,ExtensionFailure>+Send+Sync>;
pub type SessionDirProvider=Arc<dyn Fn(&ExtensionContext)->Result<PathBuf,ExtensionFailure>+Send+Sync>;
pub struct HistoryUiBindings{
    pub render:HistoryRenderer,
    pub theme:HistoryThemeMapper,
}
pub fn native_selection(bindings:HistoryUiBindings)->HistorySelection{
    let bindings=Arc::new(bindings);
    Arc::new(move|ctx,entries|{let bindings=bindings.clone();Box::pin(async move{
        let selection=ctx.ui.custom_factory(Arc::new(move|host,theme,_,done|{
            let render=(bindings.render)(host);let theme=(bindings.theme)(theme);let entries=entries.clone();
            let done=std::rc::Rc::new(move|entry:Option<types::HistoryEntry>|done(entry.map_or(JsonValue::Null,|entry|serde_json::json!(entry))));
            Box::pin(async move{Ok(Box::new(overlay::HistorySearchOverlay::new(entries,theme?,render,done)) as Box<dyn Component>)})
        }),CustomUiFactoryOptions{overlay:true,overlay_options:Some(ExtensionOverlayOptions::Static(Arc::new(||maho_tui::tui::OverlayOptions{
            width:Some(maho_tui::tui::SizeValue::Percent(90.0)),min_width:Some(60),max_height:Some(maho_tui::tui::SizeValue::Percent(80.0)),margin:Some(maho_tui::tui::OverlayMargin{top:Some(2),right:Some(2),bottom:Some(2),left:Some(2)}),..Default::default()
        }))),..Default::default()}).await?;
        if selection.is_null(){Ok(None)}else{serde_json::from_value(selection).map(Some).map_err(|error|ExtensionFailure::new(error.to_string()))}
    })})
}
pub struct HistorySearch{
    pub session_dir:SessionDirProvider,
    pub default_sessions_root:Arc<dyn Fn()->PathBuf+Send+Sync>,
    pub select:HistorySelection,
}
impl Extension for HistorySearch{
    fn register(&self,api:&mut ExtensionApi){
        let session_dir=self.session_dir.clone();let default_root=self.default_sessions_root.clone();let select=self.select.clone();
        api.register_command("history",Some("Search prompt history across sessions".into()),None,Arc::new(move|_,ctx|{let session_dir=session_dir.clone();let default_root=default_root.clone();let select=select.clone();Box::pin(async move{
            if !ctx.has_ui{ctx.ui.notify("No UI available",NotificationType::Info);return Ok(());}
            let outcome=async{
                let root=resolve_search_root(&session_dir(ctx)?,&default_root());
                tokio::task::spawn_blocking(move||indexer::index_sessions(&root)).await.map_err(|error|ExtensionFailure::new(error.to_string()))?.map_err(|error|ExtensionFailure::new(error.to_string()))
            }.await;
            let entries=match outcome{Ok(entries)=>entries,Err(error)=>{ctx.ui.notify(&format!("Failed to read prompt history: {}",error.message),NotificationType::Error);return Ok(());}};
            if entries.is_empty(){ctx.ui.notify("No prompt history found",NotificationType::Info);return Ok(());}
            if let Some(selected)=select(ctx.clone(),entries).await?{ctx.ui.set_editor_text(&selected.text);}
            Ok(())
        })}));
    }
}

