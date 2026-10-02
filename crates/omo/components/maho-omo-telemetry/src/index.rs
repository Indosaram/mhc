use std::{path::PathBuf,sync::Arc,time::Duration};
use chrono::{DateTime,Utc};
use maho_ext_api::{EventKind,EventResult,Extension,ExtensionApi};
use telemetry_core::{TelemetryEnv,TelemetryOsProvider,TelemetryTransportFactory,TelemetryProductConfig,TelemetryError,RecordDailyActiveInput,record_daily_active,DEFAULT_POSTHOG_API_KEY,DEFAULT_POSTHOG_HOST};
pub const SENPI_TELEMETRY_EVENT_NAME:&str="omo_senpi_daily_active";
pub const SENPI_MACHINE_ID_PREFIX:&str="omo-senpi:";
#[derive(Clone,Default)]
pub struct SenpiTelemetryOptions {pub env:Option<TelemetryEnv>,pub now:Option<DateTime<Utc>>,pub os_provider:Option<Arc<dyn TelemetryOsProvider>>,pub state_dir:Option<PathBuf>,pub timeout_ms:Option<u64>,pub transport_factory:Option<TelemetryTransportFactory>,pub set_timeout_fn:Option<telemetry_core::EventTelemetrySetTimeout>}
pub fn create_senpi_telemetry_product_config() -> TelemetryProductConfig {TelemetryProductConfig {cache_dir_name:"omo-senpi".into(),default_api_key:DEFAULT_POSTHOG_API_KEY.into(),default_host:DEFAULT_POSTHOG_HOST.into(),event_name:SENPI_TELEMETRY_EVENT_NAME.into(),machine_id_prefix:SENPI_MACHINE_ID_PREFIX.into(),package_name:"@oh-my-opencode/omo-senpi".into(),package_version:"5.0.0-beta.9".into(),platform:"omo-senpi".into(),product_env_prefix:"OMO_SENPI".into(),product_name:"omo-senpi".into(),..Default::default()}}
pub fn get_senpi_telemetry_state_dir(env:&TelemetryEnv) -> PathBuf {
    let agent=["OMO_CODING_AGENT_DIR","SENPI_CODING_AGENT_DIR","PI_CODING_AGENT_DIR"].iter().find_map(|name|env.get(*name).map(|s|s.trim()).filter(|s|!s.is_empty()));
    let path=match agent {Some(s)=>PathBuf::from(s),None=>PathBuf::from(env.get("HOME").cloned().unwrap_or_default()).join(".maho/agent")};
    let path=if path.is_absolute() {path} else {std::env::current_dir().unwrap_or_default().join(path)};
    path.join("omo-senpi/posthog")
}
pub async fn record_senpi_daily_active(options:&SenpiTelemetryOptions) -> Result<(),TelemetryError> {
    let options=options.clone();
    let timeout=Duration::from_millis(options.timeout_ms.unwrap_or(500));
    let mut work=tokio::spawn(async move {
    let env=options.env.clone().unwrap_or_else(||std::env::vars().collect());
    let product=create_senpi_telemetry_product_config();
    let state_dir=options.state_dir.clone().unwrap_or_else(||get_senpi_telemetry_state_dir(&env));
    let input=RecordDailyActiveInput {diagnostics:None,env:Some(&env),now:options.now,os_provider:options.os_provider.as_deref(),product:&product,reason:"session_start",source:"senpi-extension",state_dir:&state_dir,transport_factory:options.transport_factory.clone()};
    record_daily_active(&input).await
    });
    tokio::select! {
        result=&mut work=>result.map_err(|e|TelemetryError::new(e.to_string()))?,
        ()=tokio::time::sleep(timeout)=>Ok(()),
    }
}
#[derive(Default)]
pub struct SenpiTelemetryComponent {pub options:SenpiTelemetryOptions}
impl Extension for SenpiTelemetryComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let options=self.options.clone();
        api.on(EventKind::SessionStart,Arc::new(move |_,_| {let options=options.clone();Box::pin(async move {tokio::spawn(async move {if let Err(error)=record_senpi_daily_active(&options).await {eprintln!("omo-senpi telemetry failed: {error}");}});Ok(EventResult::None)})}));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Mutex,path::Path};
    use telemetry_core::{TelemetryTransport,TelemetryCaptureMessage,TelemetryCpuInfo,get_telemetry_activity_state_file_path};
    use futures::future::BoxFuture;
    struct Os;
    impl TelemetryOsProvider for Os {fn arch(&self)->String {"arm64".into()} fn cpus(&self)->Result<Vec<TelemetryCpuInfo>,TelemetryError> {Ok(vec![TelemetryCpuInfo {model:"Test CPU".into()}])} fn hostname(&self)->String {"fixture".into()} fn platform(&self)->String {"darwin".into()} fn release(&self)->String {"26.3.1".into()} fn totalmem(&self)->u64 {128*1024*1024*1024} fn os_type(&self)->String {"Darwin".into()}}
    struct Recorder(Arc<Mutex<Vec<TelemetryCaptureMessage>>>);
    impl TelemetryTransport for Recorder {fn capture(&self,m:&TelemetryCaptureMessage)->Result<(),TelemetryError> {self.0.lock().unwrap().push(m.clone());Ok(())} fn flush(&self)->Option<BoxFuture<'_,Result<(),TelemetryError>>> {Some(Box::pin(async {Ok(())}))} fn shutdown(&self)->BoxFuture<'_,Result<(),TelemetryError>> {Box::pin(async {Ok(())})}}
    fn options(home:&Path) -> (SenpiTelemetryOptions,Arc<Mutex<Vec<TelemetryCaptureMessage>>>) {let messages=Arc::new(Mutex::new(Vec::new()));let captured=Arc::clone(&messages);let env=TelemetryEnv::from([("POSTHOG_API_KEY".into(),"fixture-key".into()),("SENPI_CODING_AGENT_DIR".into(),home.to_string_lossy().into_owned())]);(SenpiTelemetryOptions {env:Some(env),now:Some(DateTime::parse_from_rfc3339("2026-07-03T04:05:06Z").unwrap().with_timezone(&Utc)),os_provider:Some(Arc::new(Os)),transport_factory:Some(Arc::new(move |_,_|Ok(Box::new(Recorder(Arc::clone(&captured)))))),..Default::default()},messages)}
    #[tokio::test] async fn first_daily_event() {let t=tempfile::tempdir().unwrap();let (o,m)=options(t.path());record_senpi_daily_active(&o).await.unwrap();assert_eq!(m.lock().unwrap()[0].event,SENPI_TELEMETRY_EVENT_NAME);}
    #[tokio::test] async fn payload_matches_imported_builder() {use telemetry_core::{CreateTelemetryClientInput,TrackActiveInput,create_telemetry_client,get_telemetry_distinct_id};let t=tempfile::tempdir().unwrap();let (o,m)=options(t.path());record_senpi_daily_active(&o).await.unwrap();let expected=Arc::new(Mutex::new(Vec::new()));let captured=Arc::clone(&expected);let product=create_senpi_telemetry_product_config();let factory:TelemetryTransportFactory=Arc::new(move |_,_|Ok(Box::new(Recorder(Arc::clone(&captured)))));let client=create_telemetry_client(&CreateTelemetryClientInput {diagnostics:None,env:o.env.as_ref(),os_provider:Some(&Os),product:&product,source:"senpi-extension",transport_factory:Some(factory)});client.track_active(&TrackActiveInput {day_utc:"2026-07-03".into(),distinct_id:get_telemetry_distinct_id(SENPI_MACHINE_ID_PREFIX,&Os),reason:"session_start".into()});client.flush().await.unwrap();client.shutdown().await;assert_eq!(*m.lock().unwrap(),*expected.lock().unwrap());}
    struct Hanging {captured:Arc<tokio::sync::Notify>}
    impl TelemetryTransport for Hanging {fn capture(&self,_:&TelemetryCaptureMessage)->Result<(),TelemetryError> {self.captured.notify_one();Ok(())}fn flush(&self)->Option<BoxFuture<'_,Result<(),TelemetryError>>> {Some(Box::pin(futures::future::pending()))}fn shutdown(&self)->BoxFuture<'_,Result<(),TelemetryError>> {Box::pin(futures::future::pending())}}
    #[tokio::test] async fn hanging_transport_does_not_block_hook() {use crate::telemetry_test_support::*;let t=tempfile::tempdir().unwrap();let (mut o,_)=options(t.path());let captured=Arc::new(tokio::sync::Notify::new());let signal=Arc::clone(&captured);o.transport_factory=Some(Arc::new(move |_,_|Ok(Box::new(Hanging {captured:Arc::clone(&signal)}))));let mut api=api();SenpiTelemetryComponent {options:o}.register(&mut api);let event=maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {reason:maho_ext_api::SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});let notification=captured.notified();dispatch(&api,event,&context(t.path(),"s")).await;tokio::time::timeout(Duration::from_secs(2),notification).await.unwrap();}
    #[tokio::test] async fn explicit_state_missing_agent_env_safe() {let t=tempfile::tempdir().unwrap();let (mut o,m)=options(t.path());o.env=Some(TelemetryEnv::from([("POSTHOG_API_KEY".into(),"fixture-key".into())]));o.state_dir=Some(t.path().into());record_senpi_daily_active(&o).await.unwrap();assert_eq!(m.lock().unwrap().len(),1);}
    #[tokio::test] async fn same_day_suppressed() {let t=tempfile::tempdir().unwrap();let (o,m)=options(t.path());record_senpi_daily_active(&o).await.unwrap();record_senpi_daily_active(&o).await.unwrap();assert_eq!(m.lock().unwrap().len(),1);}
    async fn optout(key:&str,value:&str) {let t=tempfile::tempdir().unwrap();let (mut o,m)=options(t.path());o.env.as_mut().unwrap().insert(key.into(),value.into());record_senpi_daily_active(&o).await.unwrap();assert!(m.lock().unwrap().is_empty());assert!(!get_telemetry_activity_state_file_path(&get_senpi_telemetry_state_dir(o.env.as_ref().unwrap())).exists());}
    #[tokio::test] async fn product_disable() {optout("OMO_SENPI_DISABLE_POSTHOG","1").await;}
    #[tokio::test] async fn anonymous_disable() {optout("OMO_SENPI_SEND_ANONYMOUS_TELEMETRY","0").await;}
    #[tokio::test] async fn global_disable() {optout("OMO_DISABLE_POSTHOG","1").await;}
    #[tokio::test] async fn isolated_stamp() {let t=tempfile::tempdir().unwrap();let (o,_)=options(t.path());record_senpi_daily_active(&o).await.unwrap();let dir=get_senpi_telemetry_state_dir(o.env.as_ref().unwrap());assert!(dir.starts_with(t.path()));let v:serde_json::Value=serde_json::from_str(&std::fs::read_to_string(get_telemetry_activity_state_file_path(&dir)).unwrap()).unwrap();assert_eq!(v["lastActiveDayUTC"],"2026-07-03");}
}
