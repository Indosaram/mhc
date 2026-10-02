use crate::{wiring_types::{MemoryWiringOptions,MemoryAfterBindInput},wiring_runtime::MemoryRuntimeWiring,wiring_reflection_live::MemoryReflectionLiveWiring,status_live::MemoryFooterUi};
pub struct MemoryWiring{pub runtime:MemoryRuntimeWiring,pub reflection_live:MemoryReflectionLiveWiring,pub skills_usage:crate::skills_usage_wiring::SkillsUsageTrackers,evaluators:Vec<Box<dyn crate::shutdown_drain::ShutdownEvaluator>>}
pub fn create_memory_wiring(options:MemoryWiringOptions)->MemoryWiring{MemoryWiring{runtime:options.runtime,reflection_live:Default::default(),skills_usage:options.skills_usage,evaluators:vec![]}}
struct NativeFooter(std::sync::Arc<dyn maho_ext_api::ExtensionUi>);
impl MemoryFooterUi for NativeFooter{fn set_status(&mut self,key:&str,text:&str){self.0.set_status(key,Some(text));}}
impl MemoryWiring{
    pub fn register_native(
        wiring:std::sync::Arc<tokio::sync::Mutex<Self>>,component:&std::sync::Arc<crate::index::MemoryComponent>,
        api:&mut maho_ext_api::ExtensionApi,options:crate::wiring_types::NativeMemoryWiringOptions,
        register_static:impl FnOnce(&mut maho_ext_api::ExtensionApi),
    )->Result<bool,String>{
        let handle=std::sync::Arc::new(maho_ext_api::ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone()));
        let bind_wiring=wiring.clone();let bind_handle=handle.clone();let facts=options.facts;let reconcile=options.reconcile;let now=options.now.clone();let journal_warn=options.warn.clone();
        let shutdown_wiring=wiring.clone();let shutdown_now=options.now.clone();let shutdown_warn=options.warn.clone();
        let registered=component.register(api,crate::index::MemoryComponentHooks{
            after_bind:std::sync::Arc::new(move|session,identity,context|{
                let wiring=bind_wiring.clone();let api=bind_handle.clone();let identity=identity.clone();let facts=facts(&identity,context);let reconcile=reconcile.clone();let now=now.clone();let warn=journal_warn.clone();
                Box::pin(async move{
                    let entries=context.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>();
                    let mut wiring=wiring.lock().await;let mut ui=NativeFooter(context.ui.clone());
                    let mut api=maho_ext_api::ExtensionApi::new(api.registered.clone(),api.profile.clone(),api.events.clone(),api.runtime.clone());
                    let work=reconcile(identity.clone());
                    wiring.after_bind(&mut api,MemoryAfterBindInput{session_id:session,identity,entries:&entries,now_ms:now() as i64},Some(&mut ui),||work,facts?,|session,error|warn(&format!("journal reconcile failed: {session}: {error}"))).await
                })
            }),
            shutdown:std::sync::Arc::new(move|event,context,deadline|{
                let wiring=shutdown_wiring.clone();let now=shutdown_now.clone();let warn=shutdown_warn.clone();
                Box::pin(async move{
                    let maho_ext_api::ExtensionEvent::SessionShutdown(event)=event else{return Err("memory shutdown hook received another event".into());};
                    let reason=match event.reason{
                        maho_ext_api::SessionReason::Quit=>crate::shutdown_drain::ShutdownReason::Quit,
                        maho_ext_api::SessionReason::Reload=>crate::shutdown_drain::ShutdownReason::Reload,
                        maho_ext_api::SessionReason::New=>crate::shutdown_drain::ShutdownReason::New,
                        maho_ext_api::SessionReason::Resume=>crate::shutdown_drain::ShutdownReason::Resume,
                        maho_ext_api::SessionReason::Fork=>crate::shutdown_drain::ShutdownReason::Fork,
                        maho_ext_api::SessionReason::Startup=>return Err("startup is not a shutdown reason".into()),
                    };
                    wiring.lock().await.on_session_shutdown(reason,context.session_manager.session_id(),deadline,now.as_ref(),&mut |step,error|warn(&format!("memory shutdown {step}: {}",error.unwrap_or("deadline reached")))).await;
                    Ok(())
                })
            }),
            clear_status:std::sync::Arc::new(|context|context.ui.set_status(crate::status::MEMORY_STATUS_KEY,None)),warn:options.warn.clone(),
        },register_static)?;
        if !registered{return Ok(false);}
        let now=options.now;let warn=options.warn;
        api.on(maho_ext_api::EventKind::AgentSettled,std::sync::Arc::new(move|_,context|{
            let wiring=wiring.clone();let api=handle.clone();let now=now.clone();let warn=warn.clone();
            Box::pin(async move{
                let entries=context.session_manager.get_branch().into_iter().map(|entry|entry.data).collect::<Vec<_>>();let mut ui=NativeFooter(context.ui.clone());
                wiring.lock().await.on_settled(context.session_manager.session_id(),&entries,&api,now() as i64,Some(&mut ui),|session,error|warn(&format!("journal reconcile failed: {session}: {error}"))).map_err(maho_ext_api::ExtensionFailure::new)?;
                Ok(maho_ext_api::EventResult::None)
            })
        }));Ok(true)
    }
    pub async fn after_bind<F:std::future::Future<Output=Result<(),String>>>(
        &mut self,api:&mut maho_ext_api::ExtensionApi,input:MemoryAfterBindInput<'_>,ui:Option<&mut (dyn MemoryFooterUi+Send)>,
        reconcile:impl FnOnce()->F,facts:crate::facts_wiring::MemoryFactsWiringOptions,warn:impl FnOnce(&str,&memory_core::journal::store::JournalError),
    )->Result<(),String>{
        self.runtime.contexts.insert(input.session_id.into(),input.identity.clone());self.reflection_live.attach(input.session_id);
        crate::policy_guard::register_memory_filesystem_policy(Some(api),Some(&input.identity)).map_err(|error|error.to_string())?;
        reconcile().await?;
        if !input.entries.is_empty(){self.runtime.journal_wiring_for(&input.identity).reconcile_session(Some(input.session_id),input.entries,warn).map_err(|error|error.to_string())?;}
        self.runtime.facts_wiring_for(&input.identity,||facts).reconcile_extractor();
        self.reflection_live.bind(input.session_id,input.identity,api,input.now_ms,ui.map(|ui|ui as &mut dyn MemoryFooterUi))
    }
    pub fn flush_skills_usage(&mut self,now:&dyn Fn()->String,aborted:Option<&dyn Fn()->bool>,warn:&dyn Fn(&str)){
        for tracker in self.skills_usage.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values_mut(){if aborted.is_some_and(|aborted|aborted()){return;}tracker.flush(now,aborted,warn);}
    }
    pub fn clear_status(&mut self){self.reflection_live.clear_status();}
    pub fn on_settled(
        &mut self,session:&str,entries:&[serde_json::Value],api:&maho_ext_api::ExtensionApi,now:i64,
        ui:Option<&mut dyn MemoryFooterUi>,warn:impl FnOnce(&str,&memory_core::journal::store::JournalError),
    )->Result<(),String>{
        let Some(identity)=self.runtime.resolve_context(session).cloned()else{return Ok(());};
        if !entries.is_empty(){
            self.runtime.journal_wiring_for(&identity).reconcile_session(Some(session),entries,warn).map_err(|error|error.to_string())?;
            if let Some(facts)=self.runtime.existing_facts_wiring(&identity.identity){facts.on_settled(session);}
        }
        self.reflection_live.on_settled(session,api,now,ui)
    }
    pub fn shutdown(&mut self,session:&str){let identity=self.runtime.resolve_context(session).map(|context|context.identity.clone());self.reflection_live.shutdown(identity.as_deref());}
    pub async fn on_session_shutdown(&mut self,reason:crate::shutdown_drain::ShutdownReason,session:&str,deadline:f64,now:&(dyn Fn()->f64+Sync),warn:&mut (dyn FnMut(&str,Option<&str>)+Send)){
        self.shutdown(session);
        let evaluators=std::mem::take(&mut self.evaluators);
        let evaluators={let mut drain=crate::shutdown_drain::create_shutdown_drain(MemoryShutdownSteps{wiring:self,now});for evaluator in evaluators{drain.register_evaluator(evaluator);}drain.run(reason,session,deadline,now,warn).await;std::mem::take(&mut drain.evaluators)};
        self.evaluators=evaluators;
    }
    pub fn register_shutdown_evaluator(&mut self,evaluator:Box<dyn crate::shutdown_drain::ShutdownEvaluator>){self.evaluators.push(evaluator);}
    pub fn register_dream_triggers(&mut self,api:&mut maho_ext_api::ExtensionApi,dream:std::sync::Arc<crate::dream_trigger::DreamTriggerWiring>){
        dream.register(api);self.register_shutdown_evaluator(dream.shutdown_evaluator());
    }
}
struct MemoryShutdownSteps<'a>{wiring:&'a mut MemoryWiring,now:&'a (dyn Fn()->f64+Sync)}
impl crate::shutdown_drain::ShutdownDrainSteps for MemoryShutdownSteps<'_>{
    fn flush_journal<'a>(&'a mut self,session:&'a str,signal:crate::shutdown_drain::ShutdownSignal)->crate::shutdown_drain::ShutdownWork<'a>{Box::pin(async move{let Some(identity)=self.wiring.runtime.resolve_context(session).cloned()else{return Ok(());};self.wiring.runtime.journal_wiring_for(&identity).journal_for(session).flush(Some(&||signal.aborted())).map_err(|error|error.to_string())})}
    fn enqueue_final_delta<'a>(&'a mut self,session:&'a str,signal:crate::shutdown_drain::ShutdownSignal)->crate::shutdown_drain::ShutdownWork<'a>{Box::pin(async move{if signal.aborted(){return Ok(());}let Some(identity)=self.wiring.runtime.resolve_context(session).map(|context|context.identity.clone())else{return Ok(());};if let Some(facts)=self.wiring.runtime.existing_facts_wiring(&identity){facts.enqueue_settled(session,Some(signal.facts_signal()));}Ok(())})}
    fn flush_skills_usage<'a>(&'a mut self,_:&'a str,signal:crate::shutdown_drain::ShutdownSignal)->crate::shutdown_drain::ShutdownWork<'a>{Box::pin(async move{let now=chrono::DateTime::from_timestamp_millis((self.now)() as i64).ok_or("Invalid shutdown clock")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);let failure=std::cell::RefCell::new(None);self.wiring.flush_skills_usage(&||now.clone(),Some(&||signal.aborted()),&|message|{*failure.borrow_mut()=Some(message.to_owned());});failure.into_inner().map_or(Ok(()),Err)})}
    fn launch_facts<'a>(&'a mut self,session:&'a str,signal:crate::shutdown_drain::ShutdownSignal)->crate::shutdown_drain::ShutdownWork<'a>{Box::pin(async move{if signal.aborted(){return Ok(());}let Some(identity)=self.wiring.runtime.resolve_context(session).map(|context|context.identity.clone())else{return Ok(());};if let Some(facts)=self.wiring.runtime.existing_facts_wiring(&identity){facts.launch_if_threshold_met(Some(signal.facts_signal())).await;}Ok(())})}
}
#[cfg(test)]mod tests{
    use super::*;
    struct Evaluator(std::sync::Arc<std::sync::atomic::AtomicUsize>);
    impl crate::shutdown_drain::ShutdownEvaluator for Evaluator{
        fn evaluate<'a>(&'a mut self,input:crate::shutdown_drain::ShutdownEvaluatorInput<'a>)->crate::shutdown_drain::ShutdownWork<'a>{Box::pin(async move{assert_eq!(input.reason,crate::shutdown_drain::ShutdownReason::Quit);assert_eq!(input.session_id,"session");assert!(!input.signal.aborted());self.0.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(())})}
    }
    #[tokio::test]async fn registered_evaluator_survives_reload_and_runs_on_each_quit(){
        let calls=std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));let mut wiring=create_memory_wiring(MemoryWiringOptions{runtime:Default::default(),skills_usage:Default::default()});wiring.register_shutdown_evaluator(Box::new(Evaluator(calls.clone())));
        wiring.on_session_shutdown(crate::shutdown_drain::ShutdownReason::Reload,"session",1500.0,&||0.0,&mut |step,error|panic!("{step}: {error:?}")).await;assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),0);
        for count in 1..=2{wiring.on_session_shutdown(crate::shutdown_drain::ShutdownReason::Quit,"session",1500.0,&||0.0,&mut |step,error|panic!("{step}: {error:?}")).await;assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst),count);}
    }
    #[tokio::test]async fn registered_dream_triggers_run_shutdown_dream_only_on_quit(){
        let calls=std::sync::Arc::new(std::sync::Mutex::new(vec![]));let seen=calls.clone();
        let dream=std::sync::Arc::new(crate::dream_trigger::DreamTriggerWiring::new(crate::dream_trigger::DreamTriggerOptions{
            resolve_session:std::sync::Arc::new(|_|None),resolve_active_session:std::sync::Arc::new(||None),resolve_settings:std::sync::Arc::new(|_|Default::default()),
            launch:std::sync::Arc::new(move|session,origin,request,signal|{seen.lock().unwrap().push((session,origin,request.deadline_at,signal));Box::pin(async{Ok(crate::dream_trigger::DreamFireOutcome::Fired{run_id:"run".into(),status:"active".into()})})}),warn:std::sync::Arc::new(|error|panic!("{error}")),
        }));
        let mut wiring=create_memory_wiring(MemoryWiringOptions{runtime:Default::default(),skills_usage:Default::default()});
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        wiring.register_dream_triggers(&mut api,dream);assert!(api.registered.handlers.contains_key(&maho_ext_api::EventKind::SessionAbort));
        wiring.on_session_shutdown(crate::shutdown_drain::ShutdownReason::Reload,"session",1500.0,&||0.0,&mut |step,error|panic!("{step}: {error:?}")).await;
        assert!(calls.lock().unwrap().is_empty());
        wiring.on_session_shutdown(crate::shutdown_drain::ShutdownReason::Quit,"session",1500.0,&||0.0,&mut |step,error|panic!("{step}: {error:?}")).await;
        let calls=calls.lock().unwrap();assert_eq!(calls.len(),1);assert_eq!(calls[0].0,"session");assert_eq!(calls[0].1,memory_core::reflection::DreamOrigin::Shutdown);assert_eq!(calls[0].2,Some(1500.0));assert!(calls[0].3.as_ref().unwrap().is_aborted());
    }
    #[tokio::test]async fn bind_reconciles_before_journal_and_facts_then_registers_policy(){
        let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");let identity=crate::context::MemoryIdentityContext::new("agent".into(),paths.clone(),crate::binding::create_memory_binding("agent","repo",0.0));
        let mut wiring=create_memory_wiring(MemoryWiringOptions{runtime:Default::default(),skills_usage:Default::default()});
        let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        let entries=vec![serde_json::json!({"type":"message","id":"one","message":{"role":"user","content":"hello"}})];
        let facts=crate::facts_wiring::MemoryFactsWiringOptions{identity:"agent".into(),identity_paths:paths.clone(),facts_enabled:Box::new(||false),debounce_settles:Box::new(||4),extractor:None,now:None,warn:std::sync::Arc::new(|message|panic!("{message}"))};
        wiring.after_bind(&mut api,MemoryAfterBindInput{session_id:"session",identity,entries:&entries,now_ms:0},None,||async{assert!(!paths.transcripts.join("session").exists());Ok(())},facts,|_,error|panic!("{error}")).await.unwrap();
        assert_eq!(api.registered.filesystem_policies.len(),1);assert!(wiring.runtime.resolve_context("session").is_some());assert!(paths.transcripts.join("session").exists());assert!(!paths.repo.exists());
    }
    #[tokio::test]async fn settled_reconciles_journal_before_facts_delta_and_empty_branch_does_not_enqueue(){
        let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");
        let identity=crate::context::MemoryIdentityContext::new("agent".into(),paths.clone(),crate::binding::create_memory_binding("agent","repo",0.0));
        let mut wiring=create_memory_wiring(MemoryWiringOptions{runtime:Default::default(),skills_usage:Default::default()});
        wiring.runtime.contexts.insert("session".into(),identity.clone());
        wiring.runtime.facts_wiring_for(&identity,||crate::facts_wiring::MemoryFactsWiringOptions{identity:"agent".into(),identity_paths:paths.clone(),facts_enabled:Box::new(||true),debounce_settles:Box::new(||4),extractor:None,now:Some(std::sync::Arc::new(||0)),warn:std::sync::Arc::new(|error|panic!("{error}"))});
        let api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("memory",Default::default(),Default::default()),Default::default(),Default::default(),Default::default());
        wiring.on_settled("session",&[],&api,0,None,|_,error|panic!("{error}")).unwrap();
        assert!(!paths.transcripts.join("session").exists());
        let entries=vec![serde_json::json!({"type":"message","id":"user","message":{"role":"user","content":"question"}}),serde_json::json!({"type":"message","id":"assistant","message":{"role":"assistant","content":"answer"}})];
        wiring.on_settled("session",&entries,&api,0,None,|_,error|panic!("{error}")).unwrap();
        let pending=wiring.runtime.existing_facts_wiring("agent").unwrap().reconcile_pending();
        assert_eq!(pending.len(),1);assert_eq!(pending[0].conversation_id,"session");
        wiring.on_settled("session",&entries,&api,0,None,|_,error|panic!("{error}")).unwrap();
        assert_eq!(wiring.runtime.existing_facts_wiring("agent").unwrap().reconcile_pending().len(),1);
    }
}
