use std::{path::PathBuf,sync::Arc};
use maho_ext_api::ExtensionUi;
pub const FRAME_INTERVAL_MS:u64=320;
pub const ULW_LOOP_FOOTER_FRAMES:[&str;4]=["⚡ ultraworking⠀⠀⠀","⚡ ultraworking.⠀⠀","⚡ ultraworking..⠀","⚡ ultraworking..."];
pub struct FooterRuntime { pub ui:Arc<dyn ExtensionUi>,pub goal_paths:Vec<PathBuf> }
pub fn goal_active(runtime:&FooterRuntime)->bool {
    for path in &runtime.goal_paths {
        if let Ok(bytes)=std::fs::read(path) && let Ok(value)=serde_json::from_slice::<serde_json::Value>(&bytes) && value["version"]==1 && value["goal"].is_object() && value["goal"]["status"].is_string() { return value["goal"]["status"]=="active"; }
    }
    false
}
#[derive(Default)]
pub struct FooterStatus { runtime:Option<FooterRuntime>,frame:usize,published:bool,active:bool,pub running:bool,timer:Option<tokio::task::JoinHandle<()>> }
pub fn sync_shared(footer:&Arc<std::sync::Mutex<FooterStatus>>,ctx:&maho_ext_api::ExtensionContext,active:bool) {
    let mut state=footer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    state.sync_context(ctx,active);
    if !state.running||state.timer.is_some() {return;}
    let weak=Arc::downgrade(footer);
    state.timer=Some(tokio::spawn(async move {
        let mut interval=tokio::time::interval(std::time::Duration::from_millis(FRAME_INTERVAL_MS));interval.tick().await;
        loop {
            interval.tick().await;
            let Some(footer)=weak.upgrade() else {break;};
            let mut state=footer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);state.tick();if !state.running {break;}
        }
    }));
}
impl Drop for FooterStatus {fn drop(&mut self){if let Some(timer)=self.timer.take(){timer.abort();}}}
impl FooterStatus {
    pub fn sync_context(&mut self,ctx:&maho_ext_api::ExtensionContext,active:bool) {
        let id=ctx.session_manager.session_id();let mut encoded=String::new();
        for byte in id.bytes() {if byte.is_ascii_alphanumeric()||b"-_.!~*'()".contains(&byte) {encoded.push(char::from(byte));}else{encoded.push_str(&format!("%{byte:02X}"));}}
        let mut paths=Vec::new();
        if let Some(file)=&ctx.goal_store_file {paths.push(file.clone());}
        paths.push(ctx.cwd.join(".omo/goal").join(format!("{encoded}.json")));
        self.sync(Some(FooterRuntime{ui:Arc::clone(&ctx.ui),goal_paths:paths}),active);
    }
    pub fn sync(&mut self,runtime:Option<FooterRuntime>,active:bool) {
        if runtime.is_some() {self.runtime=runtime;}self.active=active;
        if self.runtime.as_ref().is_none_or(|r|!active||!goal_active(r)) {self.stop();return;}
        if self.running {return;}self.publish();self.running=true;
    }
    pub fn tick(&mut self) {
        if self.runtime.as_ref().is_none_or(|r|!self.active||!goal_active(r)) {self.stop();return;}
        self.frame=(self.frame+1)%ULW_LOOP_FOOTER_FRAMES.len();self.publish();
    }
    pub fn dispose(&mut self) {self.active=false;self.stop();self.runtime=None;}
    fn publish(&mut self) { if let Some(runtime)=&self.runtime {runtime.ui.set_status("ulw-loop",Some(ULW_LOOP_FOOTER_FRAMES[self.frame]));self.published=true;} }
    fn stop(&mut self) {if let Some(timer)=self.timer.take(){timer.abort();}self.running=false;if self.published && let Some(runtime)=&self.runtime {runtime.ui.set_status("ulw-loop",None);}self.published=false;self.frame=0;}
}
