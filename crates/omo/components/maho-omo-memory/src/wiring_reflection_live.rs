use std::collections::BTreeMap;
use crate::{context::MemoryIdentityContext,memory_rpc_bridge::{MemoryRpcBridge,MemoryRpcBridgeDeps},status_active_runs::{ActiveReflectionRun,ActiveReflectionRunDetails,ActiveReflectionRuns},status_live::MemoryFooterUi,status_live_wiring::MemoryFooterStatusLive};
#[derive(Default)]
pub struct MemoryReflectionLiveWiring{
    pub active_runs:ActiveReflectionRuns,pub rpc:MemoryRpcBridge,pub footer:MemoryFooterStatusLive,
    session:Option<String>,contexts:BTreeMap<String,MemoryIdentityContext>,
}
struct Deps<'a>{contexts:&'a BTreeMap<String,MemoryIdentityContext>,runs:&'a ActiveReflectionRuns}
impl MemoryRpcBridgeDeps for Deps<'_>{
    fn resolve_context(&self,session:&str)->Option<&MemoryIdentityContext>{self.contexts.get(session)}
    fn active_run(&self,identity:&str)->Option<&ActiveReflectionRun>{self.runs.current(identity)}
}
impl MemoryReflectionLiveWiring{
    pub fn attach(&mut self,session:&str){self.session=Some(session.into());self.rpc.attach(session);}
    pub fn bind(&mut self,session:&str,context:MemoryIdentityContext,api:&maho_ext_api::ExtensionApi,now:i64,ui:Option<&mut dyn MemoryFooterUi>)->Result<(),String>{
        self.contexts.insert(session.into(),context);let context=self.contexts.get(session);let active=context.is_some_and(|context|self.active_runs.is_active(&context.identity));
        self.footer.sync_active(context,Some(session),active,now,ui);self.sync_rpc(api,now)
    }
    pub fn on_reflection_launched(&mut self,identity:&str,run:&str,details:ActiveReflectionRunDetails,api:&maho_ext_api::ExtensionApi,now:i64,ui:Option<&mut dyn MemoryFooterUi>)->Result<(),String>{
        self.active_runs.start(identity,run,details);self.sync_footer(now,ui);self.sync_rpc(api,now)
    }
    pub fn on_live_reflection_completed(&mut self,identity:&str,run:&str,api:&maho_ext_api::ExtensionApi,now:i64,ui:Option<&mut dyn MemoryFooterUi>)->Result<(),String>{
        self.active_runs.settle(identity,run);self.sync_footer(now,ui);self.sync_rpc(api,now)
    }
    fn sync_footer(&mut self,now:i64,ui:Option<&mut dyn MemoryFooterUi>){let session=self.session.as_deref();let context=session.and_then(|session|self.contexts.get(session));let active=context.is_some_and(|context|self.active_runs.is_active(&context.identity));self.footer.sync_active(context,session,active,now,ui);}
    pub fn on_settled(&mut self,session:&str,api:&maho_ext_api::ExtensionApi,now:i64,ui:Option<&mut dyn MemoryFooterUi>)->Result<(),String>{let context=self.contexts.get(session);let active=context.is_some_and(|context|self.active_runs.is_active(&context.identity));self.footer.refresh(context,Some(session),active,now,ui);self.sync_rpc(api,now)}
    pub fn sync_rpc(&mut self,api:&maho_ext_api::ExtensionApi,now:i64)->Result<(),String>{self.rpc.sync_native(&Deps{contexts:&self.contexts,runs:&self.active_runs},api,now)}
    pub fn shutdown(&mut self,identity:Option<&str>){if let Some(identity)=identity{self.active_runs.clear(identity);}self.footer.dispose();self.rpc.detach();}
    pub fn clear_status(&mut self){self.footer.stop();}
}
#[cfg(test)]mod tests{
    use super::*;
    #[derive(Default)]struct Ui(Vec<String>);impl MemoryFooterUi for Ui{fn set_status(&mut self,_:&str,text:&str){self.0.push(text.into());}}
    #[test]fn native_launch_completion_and_shutdown_publish_rpc_and_restore_footer(){
        let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");let engine=crate::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();let now=engine.repo.head_commit_timestamp().unwrap().unwrap()*1000;
        let context=MemoryIdentityContext::new("agent".into(),paths,crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});let api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        let events=std::sync::Arc::new(std::sync::Mutex::new(vec![]));let captured=events.clone();let _subscription=api.events.on("senpi:extension-rpc-event",std::sync::Arc::new(move |value|{match captured.lock(){Ok(mut events)=>events.push(value.clone()),Err(error)=>panic!("capture lock poisoned: {error}")}}));
        let mut wiring=MemoryReflectionLiveWiring::default();let mut ui=Ui::default();wiring.attach("session");wiring.bind("session",context,&api,now,Some(&mut ui)).unwrap();
        wiring.on_reflection_launched("agent","run",ActiveReflectionRunDetails{trigger:"manual".into(),category:"quick".into(),model:Some("p/m".into()),started_at:"now".into()},&api,now,Some(&mut ui)).unwrap();assert!(ui.0.last().unwrap().ends_with("reflecting"));
        wiring.on_live_reflection_completed("agent","run",&api,now,Some(&mut ui)).unwrap();assert_eq!(ui.0.last().unwrap(),"mem:agent just now");let events=events.lock().unwrap();assert_eq!(events.len(),3);assert_eq!(events[1]["data"]["reflection"]["activeRun"]["runId"],"run");assert!(events[2]["data"]["reflection"].get("activeRun").is_none());drop(events);
        wiring.shutdown(Some("agent"));assert!(!wiring.active_runs.is_active("agent"));let before=ui.0.len();wiring.footer.tick(&mut ui);assert_eq!(before,ui.0.len());
    }
}
