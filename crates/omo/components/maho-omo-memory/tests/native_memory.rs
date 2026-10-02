use std::sync::{Arc,Mutex};
use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::{FauxScript,FauxResponse},faux_session::FauxSession};
use maho_omo_memory::{context::MemoryIdentityContext,binding::MemorySessionBinding,prompt::*};
use memory_core::git::{GitMemoryRepo,InitializeGitRepoOptions,GitSeedFile};

struct MemoryExtension { identity:MemoryIdentityContext,captured:Arc<Mutex<Vec<String>>> }
struct RootExtension { component:Arc<maho_omo_memory::index::MemoryComponent>,bound:Arc<Mutex<Vec<String>>> }
struct WiredRootExtension{
    component:Arc<maho_omo_memory::index::MemoryComponent>,
    wiring:Arc<tokio::sync::Mutex<maho_omo_memory::wiring::MemoryWiring>>,
    reconciled:Arc<Mutex<Vec<String>>>,
}
impl Extension for WiredRootExtension{
    fn register(&self,api:&mut ExtensionApi){
        let reconciled=self.reconciled.clone();
        maho_omo_memory::wiring::MemoryWiring::register_native(self.wiring.clone(),&self.component,api,maho_omo_memory::wiring_types::NativeMemoryWiringOptions{
            facts:Arc::new(|identity,_|Ok(maho_omo_memory::facts_wiring::MemoryFactsWiringOptions{identity:identity.identity.clone(),identity_paths:identity.identity_paths.clone(),facts_enabled:Box::new(||true),debounce_settles:Box::new(||4),extractor:None,now:Some(Arc::new(||0)),warn:Arc::new(|error|panic!("{error}"))})),
            reconcile:Arc::new(move|identity|{reconciled.lock().unwrap_or_else(|error|panic!("reconcile capture: {error}")).push(identity.identity);Box::pin(async{Ok(())})}),
            now:Arc::new(||0.0),warn:Arc::new(|error|panic!("{error}")),
        },|_|{}).unwrap_or_else(|error|panic!("native wiring registration: {error}"));
    }
}
#[tokio::test]
async fn native_assembled_root_reconciles_and_enqueues_settled_journal(){
    let root=tempfile::tempdir().unwrap();let component=maho_omo_memory::index::MemoryComponent::new(maho_omo_memory::index::MemoryComponentOptions{
        env:std::collections::BTreeMap::from([("HOME".into(),root.path().to_string_lossy().into_owned())]),cwd:root.path().into(),now:Arc::new(||0.0),disabled:Arc::new(||false),load_config:Arc::new(||Ok(serde_json::json!({}))),
    });
    let wiring=Arc::new(tokio::sync::Mutex::new(maho_omo_memory::wiring::create_memory_wiring(maho_omo_memory::wiring_types::MemoryWiringOptions{runtime:Default::default(),skills_usage:Default::default()})));
    let reconciled=Arc::new(Mutex::new(vec![]));
    let session=FauxSession::new(FauxScript{name:"memory-wired-root".into(),prompt:"settled question".into(),responses:vec![FauxResponse{content:"settled answer".into(),stop_reason:"stop".into()}]}).with_native_extension(NativeExtensionFactory{path:"<memory-wired-root>".into(),source_info:Default::default(),extension:Box::new(WiredRootExtension{component,wiring:wiring.clone(),reconciled:reconciled.clone()})});
    tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let identities=reconciled.lock().unwrap().clone();assert_eq!(identities.len(),1);
    let mut wiring=wiring.lock().await;let context=wiring.runtime.contexts.values().next().unwrap().clone();
    let pending=wiring.runtime.existing_facts_wiring(&identities[0]).unwrap().reconcile_pending();assert_eq!(pending.len(),1);
    let journal=memory_core::journal::store::TranscriptJournal::new(memory_core::journal::store::TranscriptJournalOptions::new(context.identity_paths.transcripts.join(&pending[0].conversation_id)));
    assert_eq!(journal.get_state().unwrap().total_completed_steps,1);assert!(!context.repo_path().exists());
}
impl Extension for RootExtension {
    fn register(&self,api:&mut ExtensionApi){let bound=self.bound.clone();let result=self.component.register(api,maho_omo_memory::index::MemoryComponentHooks{after_bind:Arc::new(move |session,identity,_|{assert!(!identity.repo_path().exists());match bound.lock(){Ok(mut bound)=>bound.push(session.into()),Err(error)=>panic!("capture lock poisoned: {error}")};Box::pin(async{Ok(())})}),shutdown:Arc::new(|_,_,_|Box::pin(async{Ok(())})),clear_status:Arc::new(|_|{}),warn:Arc::new(|message|panic!("{message}"))},|_|{});if let Err(error)=result{panic!("root registration failed: {error}");}}
}
#[tokio::test]
async fn native_root_binds_identity_without_creating_storage(){let root=tempfile::tempdir().unwrap();let component=maho_omo_memory::index::MemoryComponent::new(maho_omo_memory::index::MemoryComponentOptions{env:std::collections::BTreeMap::from([("HOME".into(),root.path().to_string_lossy().into_owned())]),cwd:root.path().into(),now:Arc::new(||42.0),disabled:Arc::new(||false),load_config:Arc::new(||Ok(serde_json::json!({})))});let bound=Arc::new(Mutex::new(Vec::new()));let session=FauxSession::new(FauxScript{name:"memory-root".into(),prompt:"hello".into(),responses:vec![FauxResponse{content:"answer".into(),stop_reason:"stop".into()}]}).with_native_extension(NativeExtensionFactory{path:"<memory-root>".into(),source_info:Default::default(),extension:Box::new(RootExtension{component:component.clone(),bound:bound.clone()})});let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();assert_eq!(bound.lock().unwrap().len(),1);assert!(result["entries"].as_array().unwrap().iter().any(|entry|entry["customType"]==maho_omo_memory::binding::MEMORY_BINDING_CUSTOM_TYPE&&entry["data"]["boundAt"]==42.0));assert_eq!(component.supervisor.ref_count(),0);assert!(component.sessions.lock().unwrap().is_empty());}
impl Extension for MemoryExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let identity=self.identity.clone();
        register_memory_prompt_handler(api,MemoryPromptInjectionOptions {resolve_context:Arc::new(move |_|Some(identity.clone())),create_repo:None,search_exposure:None,resolve_nudge_turns:None,resolve_soul_notice:None});
        let captured=self.captured.clone();
        api.on(EventKind::BeforeAgentStart,Arc::new(move |event,_| {
            if let ExtensionEvent::BeforeAgentStart(event)=event {match captured.lock(){Ok(mut captured)=>captured.push(event.system_prompt.clone()),Err(error)=>panic!("capture lock poisoned: {error}")}}
            Box::pin(async {Ok(EventResult::None)})
        }));
    }
}

struct JournalExtension { wiring:Arc<Mutex<maho_omo_memory::journal_wiring::MemoryJournalWiring>>,sessions:Arc<Mutex<Vec<String>>> }
impl Extension for JournalExtension {
    fn register(&self,api:&mut ExtensionApi) {
        maho_omo_memory::journal_wiring::MemoryJournalWiring::register(self.wiring.clone(),api,Arc::new(|_,error|panic!("unexpected journal contention: {error}")));
        let sessions=self.sessions.clone();
        api.on(EventKind::AgentSettled,Arc::new(move |_,context| {
            match sessions.lock(){Ok(mut sessions)=>sessions.push(context.session_manager.session_id().into()),Err(error)=>panic!("session lock poisoned: {error}")}
            Box::pin(async {Ok(EventResult::None)})
        }));
    }
}

struct NudgeExtension { wiring:Arc<Mutex<maho_omo_memory::nudge_wiring::MemoryNudgeWiring>>,sessions:Arc<Mutex<Vec<String>>> }
impl Extension for NudgeExtension {
    fn register(&self,api:&mut ExtensionApi) {
        maho_omo_memory::nudge_wiring::MemoryNudgeWiring::register(self.wiring.clone(),api,Arc::new(|_|None));
        let sessions=self.sessions.clone();
        api.on(EventKind::AgentSettled,Arc::new(move |_,context| {
            match sessions.lock(){Ok(mut sessions)=>sessions.push(context.session_manager.session_id().into()),Err(error)=>panic!("session lock poisoned: {error}")}
            Box::pin(async {Ok(EventResult::None)})
        }));
    }
}

#[tokio::test]
async fn native_faux_nudge_persists_accepted_input_provenance() {
    let wiring=Arc::new(Mutex::new(maho_omo_memory::nudge_wiring::MemoryNudgeWiring::default()));
    let sessions=Arc::new(Mutex::new(Vec::new()));
    let session=FauxSession::new(FauxScript {name:"memory-nudge".into(),prompt:"accepted input".into(),responses:vec![FauxResponse {content:"answer".into(),stop_reason:"stop".into()}]}).with_native_extension(NativeExtensionFactory {path:"<memory-nudge>".into(),source_info:SourceInfo::default(),extension:Box::new(NudgeExtension {wiring:wiring.clone(),sessions:sessions.clone()})});
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let sessions=sessions.lock().unwrap(); assert_eq!(sessions.len(),1);
    assert_eq!(wiring.lock().unwrap().provenance(&sessions[0]).unwrap()["userTurns"],1);
    assert!(result["entries"].as_array().unwrap().iter().any(|entry|entry["customType"]==maho_omo_memory::nudge_wiring::ACCEPTED_TURNS_ENTRY_TYPE && entry["data"]["priorUserTurns"]==1));
}

#[tokio::test]
async fn native_faux_journal_reconciles_real_branch_on_settle() {
    let root=tempfile::tempdir().unwrap();
    let wiring=Arc::new(Mutex::new(maho_omo_memory::journal_wiring::MemoryJournalWiring::new(root.path().into())));
    let sessions=Arc::new(Mutex::new(Vec::new()));
    let session=FauxSession::new(FauxScript {name:"memory-journal".into(),prompt:"journal input".into(),responses:vec![FauxResponse {content:"journal answer".into(),stop_reason:"stop".into()}]}).with_native_extension(NativeExtensionFactory {path:"<memory-journal>".into(),source_info:SourceInfo::default(),extension:Box::new(JournalExtension {wiring:wiring.clone(),sessions:sessions.clone()})});
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let sessions=sessions.lock().unwrap(); assert_eq!(sessions.len(),1);
    let entries=result["entries"].as_array().unwrap();
    let replay=wiring.lock().unwrap().reconcile_session(Some(&sessions[0]),entries,|_,_|panic!("unexpected contention")).unwrap();
    assert_eq!(replay.appended,0); assert!(replay.skipped>0);
}

#[tokio::test]
async fn native_faux_memory_projection_equals_pinned_ts() {
    let root=tempfile::tempdir().unwrap();
    let paths=memory_core::identity::layout::build_identity_paths(root.path(),"prompt-agent");
    let repo=GitMemoryRepo::open(&paths.repo,"prompt-agent").unwrap();
    repo.init(Some(InitializeGitRepoOptions {seed_files:vec![
        GitSeedFile {relative_path:"system/persona.md".into(),content:"---\ndescription: Persona\n---\nfirst\n".into()},
        GitSeedFile {relative_path:"system/human.md".into(),content:"---\ndescription: Human\n---\nfixture person\n".into()},
        GitSeedFile {relative_path:"notes/facts/example.md".into(),content:"---\ndescription: Example\n---\nfixture fact\n".into()},
    ],..Default::default()})).unwrap();
    let captured=Arc::new(Mutex::new(Vec::new()));
    let identity=MemoryIdentityContext::new("prompt-agent".into(),paths,MemorySessionBinding {identity:"prompt-agent".into(),repo_path_hash:"fixture".into(),bound_at:0.0});
    let session=FauxSession::new(FauxScript {name:"memory-projection".into(),prompt:"hello".into(),responses:vec![FauxResponse {content:"done".into(),stop_reason:"stop".into()}]}).with_native_extension(NativeExtensionFactory {path:"<memory-projection>".into(),source_info:SourceInfo {source:"inline".into(),..Default::default()},extension:Box::new(MemoryExtension {identity,captured:captured.clone()})});
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let prompts=captured.lock().unwrap(); assert_eq!(prompts.len(),1);
    let block=memory_core::compile::render::mark_memory_block("prompt-agent",include_str!("golden/projection.txt"));
    assert!(prompts[0].contains(&block));
    assert!(result["messages"].as_array().unwrap().iter().any(|message|message["Custom"]["role"]=="custom" && message["Custom"]["customType"]==MEMORY_NOTICE_CUSTOM_TYPE && message["Custom"]["display"]==false),"native result: {result}");
}

struct MemoryToolsExtension { identity:MemoryIdentityContext,notifications:Arc<Mutex<usize>> }
impl Extension for MemoryToolsExtension {
    fn register(&self,api:&mut ExtensionApi) {
        let identity=self.identity.clone();
        maho_omo_memory::tools::register_memory_tools(api,Arc::new(move ||Some(identity.clone())));
        let state=Arc::new(Mutex::new(maho_omo_memory::wiring_memory_write::MemoryWriteSession {context:self.identity.clone(),memory_status_attempted:false}));
        let notifications=self.notifications.clone();
        maho_omo_memory::wiring_memory_write::register_memory_write_listener(api,maho_omo_memory::wiring_memory_write::MemoryWriteOptions {
            resolve_session:Arc::new(move |_|Some(state.clone())),
            on_memory_write:Arc::new(move |_| { *notifications.lock().map_err(|error|error.to_string())?+=1; Ok(()) }),
            refresh_status:Arc::new(|_,_|Ok(maho_omo_memory::status::MemoryStatusResult {notified:false,footer_shown:true})),
        });
    }
}

#[tokio::test]
async fn native_faux_memory_write_commits_and_notifies_snapshot() {
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    let root=tempfile::tempdir().unwrap(); let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");
    let identity=MemoryIdentityContext::new("agent".into(),paths.clone(),MemorySessionBinding {identity:"agent".into(),repo_path_hash:"fixture".into(),bound_at:0.0});
    let notifications=Arc::new(Mutex::new(0));
    let session=FauxSession::new(FauxScript {name:"memory-write".into(),prompt:"save".into(),responses:vec![]}).with_native_extension(NativeExtensionFactory {path:"<memory-tools>".into(),source_info:SourceInfo {source:"inline".into(),..Default::default()},extension:Box::new(MemoryToolsExtension {identity,notifications:notifications.clone()})}).with_native_responses(vec![
        faux_assistant_message(faux_tool_call("memory",serde_json::from_value(serde_json::json!({"command":"create","file_path":"notes/fact.md","description":"Fixture","file_text":"fact","reason":"save fact"})).unwrap(),Some("save-call")),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::ToolUse),timestamp:Some(0),..Default::default()}),
        faux_assistant_message("done",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}),
    ]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let messages=result["messages"].as_array().unwrap(); let tool=messages.iter().find(|message|message["role"]=="toolResult").unwrap(); assert_eq!(tool["isError"],false);
    assert_eq!(*notifications.lock().unwrap(),1,"native result: {result}");
    let repo=GitMemoryRepo::open(&paths.repo,"agent").unwrap(); let head=repo.head().unwrap().unwrap(); assert!(repo.show(&head,"notes/fact.md").unwrap().contains("fact"));
}

#[tokio::test]
async fn native_faux_corrupt_memory_reports_ts_doctor_parser_error() {
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    let root=tempfile::tempdir().unwrap(); let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");
    let engine=maho_omo_memory::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();
    std::fs::write(paths.repo.join("system/persona.md"),"broken persona without frontmatter\n").unwrap();
    let before=engine.repo.head().unwrap(); let notifications=Arc::new(Mutex::new(0));
    let identity=MemoryIdentityContext::new("agent".into(),paths.clone(),MemorySessionBinding {identity:"agent".into(),repo_path_hash:"fixture".into(),bound_at:0.0});
    let session=FauxSession::new(FauxScript {name:"memory-corrupt".into(),prompt:"edit".into(),responses:vec![]}).with_native_extension(NativeExtensionFactory {path:"<memory-corrupt>".into(),source_info:SourceInfo {source:"inline".into(),..Default::default()},extension:Box::new(MemoryToolsExtension {identity,notifications:notifications.clone()})}).with_native_responses(vec![
        faux_assistant_message(faux_tool_call("memory",serde_json::from_value(serde_json::json!({"command":"str_replace","file_path":"system/persona.md","old_string":"broken","new_string":"changed","reason":"attempt edit"})).unwrap(),Some("corrupt-call")),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::ToolUse),timestamp:Some(0),..Default::default()}),
        faux_assistant_message("done",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}),
    ]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let tool=result["messages"].as_array().unwrap().iter().find(|message|message["role"]=="toolResult").unwrap();
    assert_eq!(tool["isError"],true);
    assert!(tool["content"][0]["text"].as_str().unwrap().contains(include_str!("golden/corrupt.txt").trim()));
    assert_eq!(*notifications.lock().unwrap(),0); assert_eq!(engine.repo.head().unwrap(),before);
    assert_eq!(std::fs::read_to_string(paths.repo.join("system/persona.md")).unwrap(),"broken persona without frontmatter\n");
}
