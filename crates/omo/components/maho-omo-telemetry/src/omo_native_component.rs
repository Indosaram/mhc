use std::{path::PathBuf,sync::{Arc,Mutex}};
use maho_ext_api::{BusSubscription,EventKind,EventResult,Extension,ExtensionApi,ExtensionEvent,ExtensionFailure,SessionReason};
use crate::{index::SenpiTelemetryOptions,omo_native_parallel::ParallelTelemetryRegistry,omo_native_parallel_summary::{SummaryCapture,register_omo_native_parallel_summary},product_identity::{get_omo_native_state_dir,hash_session_id}};
pub type ConfigEnabled=Arc<dyn Fn(&std::path::Path)->bool+Send+Sync>;
pub struct OmoNativeTelemetryComponent {
    pub options:SenpiTelemetryOptions,
    pub skills_root:PathBuf,
    pub is_config_enabled:ConfigEnabled,
    pub clock:Arc<dyn Fn()->f64+Send+Sync>,
    subscriptions:Mutex<Vec<BusSubscription>>,
}
impl OmoNativeTelemetryComponent {pub fn new(options:SenpiTelemetryOptions,skills_root:PathBuf,is_config_enabled:ConfigEnabled)->Self {Self {options,skills_root,is_config_enabled,clock:Arc::new(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()*1000.0),subscriptions:Mutex::default()}}}
impl Extension for OmoNativeTelemetryComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let env=self.options.env.clone().unwrap_or_else(||std::env::vars().collect());
        let state_dir=self.options.state_dir.clone().unwrap_or_else(||get_omo_native_state_dir(&env));
        let notice_state_dir=state_dir.clone();
        let shared:Arc<Mutex<Option<telemetry_core::EventTelemetryClient>>>=Arc::default();
        let client=Arc::clone(&shared);
        let capture:SummaryCapture=Arc::new(move |name,properties| {if let Some(client)=client.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() && let Some(properties)=properties.as_object() {client.capture_event(name,properties);}});
        let hash:Arc<dyn Fn(&str)->String+Send+Sync>=Arc::new(move |id|hash_session_id(id,&state_dir).unwrap_or_else(|error| {eprintln!("omo-native session identity failed: {error}");String::new()}));
        let registry=Arc::new(Mutex::new(ParallelTelemetryRegistry::default()));
        let now=Arc::clone(&self.clock);
        let subscription=register_omo_native_parallel_summary(api,registry,now,Arc::clone(&hash),Arc::clone(&capture));
        self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(subscription);
        let options=self.options.clone();let enabled=Arc::clone(&self.is_config_enabled);let client=Arc::clone(&shared);
        api.on(EventKind::SessionStart,Arc::new(move |event,ctx| {let result=if enabled(&ctx.cwd) && let ExtensionEvent::SessionStart(event)=event {
            let reason=match event.reason {SessionReason::Startup=>"startup",SessionReason::Reload=>"reload",SessionReason::New=>"new",SessionReason::Resume=>"resume",SessionReason::Fork=>"fork",SessionReason::Quit=>"startup"};
            crate::omo_native_session::start_native_session(&options,ctx.session_manager.session_id(),&serde_json::json!({"reason":reason}),&ctx.agent_dir).map(|session|{*client.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=session;})
        } else {Ok(())};Box::pin(async move {result.map_err(|e|ExtensionFailure::new(e.to_string()))?;Ok(EventResult::None)})}));
        api.on(EventKind::SessionShutdown,Arc::new(move |_,_| {let client=shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();Box::pin(async move {if let Some(client)=client {client.shutdown().await;}Ok(EventResult::None)})}));
        crate::omo_native_turns::register_omo_native_turn_telemetry(api,Arc::clone(&hash),Arc::clone(&capture),Arc::new(||eprintln!("telemetry_event_property_rejected: turn_end assistant usage contained missing or invalid values")));
        crate::omo_native_tools::register_omo_native_tool_telemetry(api,self.skills_root.clone(),hash,capture);
        crate::omo_native_notice::register_omo_native_notice(api,env,notice_state_dir,Arc::clone(&self.is_config_enabled));
    }
}
#[cfg(test)]
mod tests {
    use super::*;use crate::telemetry_test_support::*;use telemetry_core::*;use futures::future::BoxFuture;
    struct Recorder(Arc<Mutex<Vec<TelemetryCaptureMessage>>>);
    impl TelemetryTransport for Recorder {fn capture(&self,m:&TelemetryCaptureMessage)->Result<(),TelemetryError> {self.0.lock().unwrap().push(m.clone());Ok(())}fn flush(&self)->Option<BoxFuture<'_,Result<(),TelemetryError>>> {None}fn shutdown(&self)->BoxFuture<'_,Result<(),TelemetryError>> {Box::pin(async {Ok(())})}}
    fn component(home:&std::path::Path)->(OmoNativeTelemetryComponent,Arc<Mutex<Vec<TelemetryCaptureMessage>>>) {std::fs::write(home.join("models.json"),"{\"providers\":{}}").unwrap();std::fs::write(home.join("settings.json"),"{}").unwrap();let messages=Arc::new(Mutex::new(Vec::new()));let captured=Arc::clone(&messages);let options=SenpiTelemetryOptions {env:Some(env(home)),state_dir:Some(home.join("native")),transport_factory:Some(Arc::new(move |_,_|Ok(Box::new(Recorder(Arc::clone(&captured)))))),..Default::default()};{let mut component=OmoNativeTelemetryComponent::new(options,home.join("skills"),Arc::new(|_|true));let clock=std::sync::atomic::AtomicU64::new(1000);component.clock=Arc::new(move || f64::from(u32::try_from(clock.fetch_add(1,std::sync::atomic::Ordering::SeqCst)).unwrap()));(component,messages)}}
    fn start()->ExtensionEvent {ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None})}
    fn shutdown()->ExtensionEvent {ExtensionEvent::SessionShutdown(maho_ext_api::SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None})}
    #[tokio::test] async fn shutdown_summary_precedes_client_teardown() {let t=tempfile::tempdir().unwrap();let (component,messages)=component(t.path());let mut api=api();component.register(&mut api);let ctx=context(t.path(),"ordering-session");dispatch(&api,start(),&ctx).await;for id in ["a","b"] {dispatch(&api,ExtensionEvent::ToolExecutionStart {tool_call_id:id.into(),tool_name:"bash".into(),args:serde_json::json!({})},&ctx).await;}for id in ["a","b"] {dispatch(&api,ExtensionEvent::ToolExecutionEnd {tool_call_id:id.into(),tool_name:"bash".into(),result:serde_json::json!(1),is_error:false},&ctx).await;}dispatch(&api,shutdown(),&ctx).await;dispatch(&api,shutdown(),&ctx).await;let messages=messages.lock().unwrap();let summaries:Vec<_>=messages.iter().filter(|m|m.event=="parallelism_summary").collect();assert_eq!(summaries.len(),1);assert_eq!(summaries[0].properties["non_eval_joined_calls"],2);}
    #[tokio::test] async fn disabled_config_captures_nothing() {let t=tempfile::tempdir().unwrap();let (mut component,messages)=component(t.path());component.is_config_enabled=Arc::new(|_|false);let mut api=api();component.register(&mut api);dispatch(&api,start(),&context(t.path(),"disabled")).await;assert!(messages.lock().unwrap().is_empty());assert!(!t.path().join("native/notice-shown").exists());}
    #[tokio::test] async fn restart_refreshes_session_client() {let t=tempfile::tempdir().unwrap();let (component,messages)=component(t.path());let mut api=api();component.register(&mut api);for id in ["first","second"] {let ctx=context(t.path(),id);dispatch(&api,start(),&ctx).await;dispatch(&api,shutdown(),&ctx).await;dispatch(&api,shutdown(),&ctx).await;}let messages=messages.lock().unwrap();let sessions:Vec<_>=messages.iter().filter(|m|m.event=="session_started").collect();assert_eq!(sessions.len(),2);assert_ne!(sessions[0].properties["$session_id"],sessions[1].properties["$session_id"]);}
}
