use std::sync::{Arc,Mutex};
use serde_json::Value;
use crate::{commands::PanelCommandContext,constants::{SIDE_PANEL_DIFF_COMMAND,SIDE_PANEL_FLAG},context::PanelEventContext,runtime::SidePanelController,settings::{OmoSidePanelSettings,SettingsError},types::PanelFuture};
pub type EventHandler=Arc<dyn Fn(Value,PanelEventContext)->PanelFuture<'static,()>+Send+Sync>;
pub type CommandHandler=Arc<dyn Fn(PanelCommandContext)->PanelFuture<'static,()>+Send+Sync>;
pub trait SidePanelRegistration:Send+Sync {
    fn cwd(&self)->String;
    fn flag(&self,name:&str)->Option<bool>;
    fn register_flag(&self,name:&str,description:&str);
    fn register_command(&self,name:&str,description:&str,handler:CommandHandler);
    fn on(&self,event:&str,handler:EventHandler);
    fn warn(&self,message:&str);
}
pub type RuntimeLoader=Arc<dyn Fn()->PanelFuture<'static,Result<Arc<SidePanelController>,String>>+Send+Sync>;
pub type SettingsLoader=Arc<dyn Fn(&str)->Result<OmoSidePanelSettings,SettingsError>+Send+Sync>;
pub struct SidePanelComponentOptions { pub load_settings:SettingsLoader,pub load_runtime:RuntimeLoader }
pub struct SidePanelComponent { options:SidePanelComponentOptions,state:Mutex<ShellState>,load_lock:tokio::sync::Mutex<()> }
#[derive(Default)] struct ShellState { controller:Option<Arc<SidePanelController>>,epoch:u64 }
pub fn create_side_panel_component(options:SidePanelComponentOptions)->Arc<SidePanelComponent> { Arc::new(SidePanelComponent { options,state:Mutex::default(),load_lock:tokio::sync::Mutex::new(()) }) }
impl SidePanelComponent {
    pub fn name(&self)->&'static str { "side-panel" }
    pub fn register(self:&Arc<Self>,host:Arc<dyn SidePanelRegistration>)->Result<(),SettingsError> {
        host.register_flag(SIDE_PANEL_FLAG,"Force the omo side panel on for this run; side_panel.enabled in omo.json is the persistent switch.");
        let cwd=host.cwd();let settings=(self.options.load_settings)(&cwd)?;
        if is_panel_enabled(&settings,host.flag(SIDE_PANEL_FLAG)) {
            let weak=Arc::downgrade(self);host.register_command(SIDE_PANEL_DIFF_COMMAND,"Open the diff of a file the side panel lists as changed.",Arc::new(move |ctx| {
                let controller=weak.upgrade().and_then(|shell|shell.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).controller.clone());
                Box::pin(async move { if let Some(controller)=controller { controller.run_diff_command(&ctx).await; } else if let Some(ui)=ctx.ui { ui.notify("The side panel is off in this session.","info"); } })
            }));
        }
        for event in ["session_start","turn_end","agent_settled","message_end","tool_execution_start","tool_execution_end","input","session_shutdown"] {
            let weak=Arc::downgrade(self);let event_host=host.clone();let cwd=cwd.clone();
            host.on(event,Arc::new(move |payload,ctx| { let shell=weak.upgrade();let host=event_host.clone();let cwd=cwd.clone();Box::pin(async move { if let Some(shell)=shell { shell.forward(event,payload,ctx,host,&cwd).await; } }) }));
        }
        Ok(())
    }
    pub async fn forward(&self,event:&str,payload:Value,ctx:PanelEventContext,host:Arc<dyn SidePanelRegistration>,cwd:&str) {
        if event=="session_shutdown" {
            let controller={let mut s=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);s.epoch=s.epoch.wrapping_add(1);s.controller.clone()};if let Some(controller)=controller { controller.teardown(); }return;
        }
        if event=="session_start" {
            let settings=match (self.options.load_settings)(cwd) { Ok(s)=>s,Err(error)=>{host.warn(&error.to_string());return;} };
            if !is_panel_enabled(&settings,host.flag(SIDE_PANEL_FLAG)) { return; }
            let epoch=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).epoch;
            let controller={
                let _load=self.load_lock.lock().await;
                let current=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).controller.clone();
                if let Some(controller)=current { controller } else {
                    let controller=match (self.options.load_runtime)().await { Ok(controller)=>controller,Err(error)=>{host.warn(&format!("omo-senpi side panel: implementation did not load: {error}"));return;} };
                    self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).controller=Some(controller.clone());controller
                }
            };
            if self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).epoch!=epoch { return; }
            controller.session_start(settings,&ctx).await;return;
        }
        let controller=self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).controller.clone();let Some(controller)=controller else { return; };
        match event { "turn_end"|"agent_settled"|"message_end"=>controller.refresh(ctx.value),"tool_execution_start"=>controller.tool_start(&payload,ctx.value),"tool_execution_end"=>controller.tool_end(),"input"=>controller.input(),_=>{} }
    }
}
pub fn is_panel_enabled(settings:&OmoSidePanelSettings,flag:Option<bool>)->bool { flag.unwrap_or(settings.enabled) }
#[cfg(test)] mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize,Ordering};
    #[derive(Default)]
    struct Host { flag:Mutex<Option<bool>>,flags:Mutex<Vec<String>>,commands:Mutex<Vec<String>>,events:Mutex<Vec<String>>,warnings:Mutex<Vec<String>> }
    impl SidePanelRegistration for Host {
        fn cwd(&self)->String { "/repo".into() }
        fn flag(&self,_:&str)->Option<bool> { *self.flag.lock().expect("flag") }
        fn register_flag(&self,name:&str,_:&str) { self.flags.lock().expect("flags").push(name.into()); }
        fn register_command(&self,name:&str,_:&str,_:CommandHandler) { self.commands.lock().expect("commands").push(name.into()); }
        fn on(&self,event:&str,_:EventHandler) { self.events.lock().expect("events").push(event.into()); }
        fn warn(&self,message:&str) { self.warnings.lock().expect("warnings").push(message.into()); }
    }
    #[test] fn flag_overrides_in_both_directions_without_default() { let mut settings=OmoSidePanelSettings::default();assert!(!is_panel_enabled(&settings,None));assert!(is_panel_enabled(&settings,Some(true)));settings.enabled=true;assert!(is_panel_enabled(&settings,None));assert!(!is_panel_enabled(&settings,Some(false))); }
    #[test] fn disabled_sessions_never_load_runtime_and_register_no_diff_command() { futures::executor::block_on(async {
        let host=Arc::new(Host::default());let loads=Arc::new(AtomicUsize::new(0));let counter=loads.clone();
        let component=create_side_panel_component(SidePanelComponentOptions { load_settings:Arc::new(|_|Ok(OmoSidePanelSettings::default())),load_runtime:Arc::new(move || {counter.fetch_add(1,Ordering::SeqCst);Box::pin(async {Err("not reached".into())})}) });
        component.register(host.clone()).expect("register");
        component.forward("session_start",Value::Null,PanelEventContext {value:serde_json::json!({"mode":"tui","hasUI":true}),ui:None},host.clone(),"/repo").await;
        assert_eq!(loads.load(Ordering::SeqCst),0);assert_eq!(*host.flag.lock().expect("flag"),None);assert_eq!(*host.flags.lock().expect("flags"),[SIDE_PANEL_FLAG]);assert!(host.commands.lock().expect("commands").is_empty());assert_eq!(host.events.lock().expect("events").len(),8);
    }); }
    #[test] fn flag_enables_lazy_load_and_failed_load_retries_next_session() { futures::executor::block_on(async {
        let host=Arc::new(Host::default());*host.flag.lock().expect("flag")=Some(true);let loads=Arc::new(AtomicUsize::new(0));let counter=loads.clone();
        let component=create_side_panel_component(SidePanelComponentOptions {load_settings:Arc::new(|_|Ok(OmoSidePanelSettings::default())),load_runtime:Arc::new(move || {counter.fetch_add(1,Ordering::SeqCst);Box::pin(async {Err("missing implementation".into())})})});
        component.register(host.clone()).expect("register");assert_eq!(loads.load(Ordering::SeqCst),0);assert_eq!(*host.commands.lock().expect("commands"),[SIDE_PANEL_DIFF_COMMAND]);
        for _ in 0..2 {component.forward("session_start",Value::Null,PanelEventContext {value:Value::Null,ui:None},host.clone(),"/repo").await;}
        assert_eq!(loads.load(Ordering::SeqCst),2);assert_eq!(host.warnings.lock().expect("warnings").len(),2);
    }); }
}
