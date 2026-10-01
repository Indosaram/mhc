use std::{path::PathBuf,sync::{Arc,Mutex}};
use maho_ext_api::{BusSubscription,EventKind,EventResult,Extension,ExtensionApi,ExtensionEvent,ExtensionFailure,SessionReason};
use crate::{index::SenpiTelemetryOptions,omo_native_parallel::ParallelTelemetryRegistry,omo_native_parallel_summary::{SummaryCapture,register_omo_native_parallel_summary},product_identity::{get_omo_native_state_dir,hash_session_id}};
pub type ConfigEnabled=Arc<dyn Fn(&std::path::Path)->bool+Send+Sync>;
pub struct OmoNativeTelemetryComponent {
    pub options:SenpiTelemetryOptions,
    pub skills_root:PathBuf,
    pub is_config_enabled:ConfigEnabled,
    subscriptions:Mutex<Vec<BusSubscription>>,
}
impl OmoNativeTelemetryComponent {pub fn new(options:SenpiTelemetryOptions,skills_root:PathBuf,is_config_enabled:ConfigEnabled)->Self {Self {options,skills_root,is_config_enabled,subscriptions:Mutex::default()}}}
impl Extension for OmoNativeTelemetryComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let env=self.options.env.clone().unwrap_or_else(||std::env::vars().collect());
        let state_dir=self.options.state_dir.clone().unwrap_or_else(||get_omo_native_state_dir(&env));
        let shared:Arc<Mutex<Option<telemetry_core::EventTelemetryClient>>>=Arc::default();
        let client=Arc::clone(&shared);
        let capture:SummaryCapture=Arc::new(move |name,properties| {if let Some(client)=client.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() && let Some(properties)=properties.as_object() {client.capture_event(name,properties);}});
        let hash:Arc<dyn Fn(&str)->String+Send+Sync>=Arc::new(move |id|hash_session_id(id,&state_dir).unwrap_or_else(|error| {eprintln!("omo-native session identity failed: {error}");String::new()}));
        let registry=Arc::new(Mutex::new(ParallelTelemetryRegistry::default()));
        let now=Arc::new(||std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs_f64()*1000.0);
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
    }
}
