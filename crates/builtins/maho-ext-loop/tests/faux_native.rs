use std::sync::Arc;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_ext_loop::{extension::LoopExtension,types::{LoopStoreRef,CronEntry,LoopPhase}};
use maho_test_support::{faux::FauxScript,faux_session::FauxSession};

#[tokio::test]
async fn native_faux_restores_and_shutdown_suspends_existing_loop() {
    let dir=tempfile::tempdir().unwrap();
    let reference=LoopStoreRef { base_dir:dir.path().into(),session_id:"fixture".into() }; let stored=reference.clone();
    let mut scheduler=maho_ext_loop::scheduler::LoopScheduler::new("fixture",None,&Default::default());
    scheduler.create_dynamic(maho_ext_loop::scheduler::CreateDynamicRequest { original_args:"watch changes".into(),reentry_prompt:"/loop watch changes".into(),payload:maho_ext_loop::types::LoopPayload::Prompt { prompt:"watch changes".into() } },"fixture-loop".into(),0.0);
    maho_ext_loop::store::write_loop_state(&reference,&scheduler.state).await.unwrap();
    let mut extension=LoopExtension::new(Arc::new(move |_|stored.clone()));
    extension.now=Arc::new(||0.0); extension.home=dir.path().to_string_lossy().into_owned();
    let session=FauxSession::new(FauxScript { name:"loop-native-command".into(),prompt:"/loop watch changes".into(),responses:Vec::new() })
        .with_native_extension(NativeExtensionFactory { path:"<loop>".into(),source_info:Default::default(),extension:Box::new(extension) });
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    assert_eq!(result["messages"],serde_json::json!([]));
    let state=maho_ext_loop::store::read_loop_state(&reference).await.unwrap().unwrap();
    assert_eq!(state.entries.len(),1);
    let CronEntry::Dynamic { lifecycle,fields,.. }=state.entries.values().next().unwrap() else { panic!("dynamic loop expected") };
    assert_eq!(lifecycle.phase(),LoopPhase::Suspended); assert!(lifecycle.end_reason().is_none()); assert!(fields.reentry_prompt.contains("watch changes"));
}
