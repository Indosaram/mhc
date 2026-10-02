use std::sync::Arc;
use maho_ext_api::{ExtensionApi,ExtensionFailure,EventKind,EventResult};
use std::collections::BTreeMap;
use senpi_task::dag::{store::{DagFileStore,DagEventReadOptions,DagStoreError},types::DagRunEvent};

pub type DurableDagListener=Arc<dyn Fn(&DagRunEvent)+Send+Sync>;
pub fn runtime_skill_materializer(store:Arc<DagFileStore>,cwd:Arc<dyn Fn()->String+Send+Sync>,load_skills:Arc<senpi_task::tools::task::types::SkillLoader>)->senpi_task::dag::manager::DagMaterializeSkills {
    Arc::new(move |input| senpi_task::dag::skills::create_dag_skill_materializer(senpi_task::dag::skills::DagSkillMaterializerOptions { store:store.clone(),cwd:cwd(),load_skills:Some(load_skills.clone()),home_dir:None,extra_dirs:vec![] })(input))
}
pub fn dag_activity_payload(run:&str,node:&str,task:&str,at:&str,details:&senpi_task::progress::ToolProgressDetails)->serde_json::Value {
    let mut activity=serde_json::json!({"schemaVersion":1,"runId":run,"nodeId":node,"taskId":task,"at":at,"activity":details.progress.activity,"turns":details.turns});
    if let Some(tool)=&details.current_tool { activity["currentTool"]=serde_json::json!(tool); }
    if let Some(line)=&details.last_assistant_line { activity["lastAssistantLine"]=serde_json::json!(line); }
    if let Some(calls)=details.tool_calls { activity["toolCalls"]=serde_json::json!(calls); }
    activity
}
pub fn publish_scheduler_event(event:&DagRunEvent,delivered:&mut BTreeMap<String,u64>,listeners:&BTreeMap<String,Vec<DurableDagListener>>,on_event:&DurableDagListener) {
    if *delivered.get(&event.run_id).unwrap_or(&0)>=event.seq { return; }
    delivered.insert(event.run_id.clone(),event.seq);
    deliver_durable_event(on_event,event);
    for listener in listeners.get(&event.run_id).into_iter().flatten() { deliver_durable_event(listener,event); }
}
pub fn publish_durable_events(
    store:&DagFileStore,run_id:&str,delivered:&mut BTreeMap<String,u64>,
    listeners:&BTreeMap<String,Vec<DurableDagListener>>,on_event:&DurableDagListener,
) -> Result<(),DagStoreError> {
    let mut since=*delivered.get(run_id).unwrap_or(&0);
    loop {
        let page=store.read_events(run_id,since,&DagEventReadOptions { limit:1000,..Default::default() })?;
        for event in &page.events {
            delivered.insert(run_id.into(),event.seq);
            deliver_durable_event(on_event,event);
            for listener in listeners.get(run_id).into_iter().flatten() { deliver_durable_event(listener,event); }
        }
        if !page.has_more { return Ok(()); }
        since=page.next_since_seq;
    }
}
fn deliver_durable_event(listener:&DurableDagListener,event:&DagRunEvent) {
    if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| listener(event))).is_err() {
        eprintln!("DAG runtime subscriber failed");
    }
}
pub trait DagRuntimeLifecycle:Send+Sync {
    fn attach(&self)->Result<(),ExtensionFailure>;
    fn detach(&self);
    fn pause_for_shutdown(&self)->Result<(),ExtensionFailure>;
    fn dispose(&self);
}
pub fn wire_dag_lifecycle(api:&mut ExtensionApi,runtime:Arc<dyn DagRuntimeLifecycle>,wire_task_lifecycle:impl FnOnce(&mut ExtensionApi)) {
    let pause=runtime.clone();
    api.on(EventKind::SessionShutdown,Arc::new(move |_,_| { let pause=pause.clone(); Box::pin(async move { pause.pause_for_shutdown()?; Ok(EventResult::None) }) }));
    wire_task_lifecycle(api);
    let attach=runtime.clone();
    api.on(EventKind::SessionStart,Arc::new(move |_,_| { let attach=attach.clone(); Box::pin(async move { attach.attach()?; Ok(EventResult::None) }) }));
    let detach=runtime.clone();
    api.on(EventKind::SessionBeforeSwitch,Arc::new(move |_,_| { let detach=detach.clone(); Box::pin(async move { detach.detach(); Ok(EventResult::None) }) }));
    api.on(EventKind::SessionShutdown,Arc::new(move |_,_| { let runtime=runtime.clone(); Box::pin(async move { runtime.dispose(); Ok(EventResult::None) }) }));
}
