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
pub struct FooterStatus { runtime:Option<FooterRuntime>,frame:usize,published:bool,active:bool,pub running:bool }
impl FooterStatus {
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
    fn stop(&mut self) {self.running=false;if self.published && let Some(runtime)=&self.runtime {runtime.ui.set_status("ulw-loop",None);}self.published=false;self.frame=0;}
}
