//! Port of omo-senpi `components/config-watch/index.ts` at 77f3067f1. Registers the
//! omo config surfaces on the shared `EventBus`. Initial, ready, and retry publications
//! retain the native callable consumed by `maho-ext-config-reload`.
//!
//! Logging: upstream logs through `ComponentContext.logger`. The composition (todo 47) passes a
//! `logger` here and `ConfigWatchComponent::register` builds the sink from it via [`logger_sink`];
//! the injectable `log` option still overrides for tests.
use std::{path::PathBuf, sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}}};
use maho_ext_api::{BusSubscription, ComponentLogger, ConfigWatchTargetKind, ConfigWatchTargetSpec, ConfigWatchValidation as ApiConfigWatchValidation, EventBus, EventKind, EventResult, Extension, ExtensionApi, JsonValue, RegisteredConfigWatch};
use crate::paths::{OmoConfigWatchTargetResolution, resolve_omo_config_watch_target_resolution};
use crate::validate::{ConfigWatchValidation, OmoConfigValidator};

pub const CONFIG_WATCH_REGISTER:&str="config-watch:register";
pub const CONFIG_WATCH_READY:&str="config-watch:ready";
pub const CONFIG_WATCH_RELOADED:&str="config-watch:reloaded";
pub const CONFIG_WATCH_REJECTED:&str="config-watch:rejected";
pub const OMO_REGISTRATION_ID:&str="omo";
pub const OMO_REGISTRATION_DISPLAY_NAME:&str=".omo config";
/// Caps deferred re-emits per payload: the consumer rejects on the same synchronous
/// stack as REGISTER, so an unbounded retry recurses until stack overflow.
pub const MAX_REJECTION_RETRIES:usize=3;

#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum ConfigWatchLogLevel { Debug, Info, Warn, Error }
impl ConfigWatchLogLevel {
    pub const fn as_str(&self)->&'static str { match self { Self::Debug=>"debug",Self::Info=>"info",Self::Warn=>"warn",Self::Error=>"error" } }
}

/// Stands in for `ComponentContext.logger` until the composition owner binds it.
pub type ConfigWatchLogSink=Arc<dyn Fn(ConfigWatchLogLevel,&str,Option<&JsonValue>)+Send+Sync>;
/// Adapts the shared `maho_ext_api::ComponentLogger` (owner lane/20c) onto the injectable sink.
/// `Debug` has no logger method, so it maps to `info`, matching upstream's `ctx.logger` usage.
pub fn logger_sink(logger:Arc<dyn ComponentLogger>)->ConfigWatchLogSink {
    Arc::new(move |level:ConfigWatchLogLevel,message:&str,details:Option<&JsonValue>|match level {
        ConfigWatchLogLevel::Debug|ConfigWatchLogLevel::Info=>logger.info(message,details),
        ConfigWatchLogLevel::Warn=>logger.warn(message,details),
        ConfigWatchLogLevel::Error=>logger.error(message,details),
    })
}
pub type ResolveCwd=Arc<dyn Fn()->String+Send+Sync>;
pub type ResolveTargets=Arc<dyn Fn(&str)->Vec<crate::paths::OmoConfigWatchTarget>+Send+Sync>;
pub type ResolveTargetResolution=Arc<dyn Fn(&str)->OmoConfigWatchTargetResolution+Send+Sync>;
pub type CreateValidator=Arc<dyn Fn(&str)->OmoConfigValidator+Send+Sync>;

#[derive(Clone,Default)]
pub struct ConfigWatchComponentOptions {
    pub resolve_cwd:Option<ResolveCwd>,
    pub resolve_targets:Option<ResolveTargets>,
    pub resolve_target_resolution:Option<ResolveTargetResolution>,
    pub create_validator:Option<CreateValidator>,
    /// Explicit sink; wins over `logger`. Used by tests.
    pub log:Option<ConfigWatchLogSink>,
    /// Shared component logger supplied by the composition (todo 47).
    pub logger:Option<Arc<dyn ComponentLogger>>,
}

struct Lifecycle { registration:Option<RegisteredConfigWatch>, fingerprint:String, retries:usize, warned_reload_required:bool }

pub struct ConfigWatchComponent {
    pub options:ConfigWatchComponentOptions,
    subscriptions:Arc<Mutex<Vec<BusSubscription>>>,
    validator:Mutex<Option<Arc<Mutex<OmoConfigValidator>>>>,
    retry_epoch:Arc<AtomicU64>,
}
impl Default for ConfigWatchComponent {
    fn default()->Self { Self { options:ConfigWatchComponentOptions::default(),subscriptions:Arc::default(),validator:Mutex::default(),retry_epoch:Arc::new(AtomicU64::new(0)) } }
}
impl ConfigWatchComponent {
    fn release(&self) {
        self.retry_epoch.fetch_add(1,Ordering::SeqCst);
        self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }
    /// The verdict producer upstream exposes as the registration's `validate` member;
    /// one validator instance is reused across target refreshes, keeping a rejected
    /// diagnostic sticky until its source is repaired.
    pub fn validate_changed_paths(&self,changed:&[PathBuf])->Option<ConfigWatchValidation> {
        let validator=Arc::clone(self.validator.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref()?);
        Some(validator.lock().unwrap_or_else(std::sync::PoisonError::into_inner).validate(changed))
    }
}
impl Extension for ConfigWatchComponent {
    fn register(&self,api:&mut ExtensionApi) {
        self.release();
        let cwd=match &self.options.resolve_cwd { Some(resolve)=>resolve(), None=>std::env::current_dir().map_or_else(|_|String::new(),|path|path.to_string_lossy().into_owned()) };
        let log=self.options.log.clone().or_else(||self.options.logger.clone().map(logger_sink)).unwrap_or_else(||Arc::new(|level:ConfigWatchLogLevel,message:&str,details:Option<&JsonValue>|match details {
            Some(details)=>eprintln!("config-watch {}: {message} {details}",level.as_str()),
            None=>eprintln!("config-watch {}: {message}",level.as_str()),
        }));
        let resolution=self.options.resolve_target_resolution.clone().unwrap_or_else(||{
            let resolve_targets=self.options.resolve_targets.clone();
            Arc::new(move |cwd:&str|match &resolve_targets {
                Some(resolve)=>OmoConfigWatchTargetResolution{targets:resolve(cwd),user_config_creation_watched:true,user_config_creation_discovery:"watched"},
                None=>{ let home=std::env::var("HOME").unwrap_or_default(); resolve_omo_config_watch_target_resolution(PathBuf::from(cwd).as_path(),PathBuf::from(&home).as_path(),&std::env::vars().collect()) }
            })
        });
        let create_validator=self.options.create_validator.clone().unwrap_or_else(||Arc::new(|cwd:&str|OmoConfigValidator::new(cwd.to_owned(),std::env::vars().collect())));
        let validator=Arc::new(Mutex::new(create_validator(&cwd)));
        *self.validator.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=Some(Arc::clone(&validator));
        let events=api.events.clone();
        let state=Arc::new(Mutex::new(Lifecycle{registration:None,fingerprint:String::new(),retries:0,warned_reload_required:false}));
        let resolved=resolve_with_warning(&cwd,&resolution,&log,&state);
        let registration=registration_typed(&resolved,&validator);
        state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).registration=Some(registration.clone());
        let extension_path=api.registered.identity.path.clone();

        let ready_events=events.clone();let ready_state=Arc::clone(&state);
        let ready_path=extension_path.clone();
        let ready=events.on(CONFIG_WATCH_READY,Arc::new(move|_|{
            let registration=ready_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).registration.clone();
            if let Some(registration)=registration { ready_events.publish_config_watch(&ready_path,registration); }
        }));
        let reloaded_log=Arc::clone(&log);
        let reloaded=events.on(CONFIG_WATCH_RELOADED,Arc::new(move|payload|{
            let Some(reloaded)=reloaded_payload(payload) else { return; };
            let path_count=reloaded.paths.len();
            reloaded_log(ConfigWatchLogLevel::Info,"omo config hot-reloaded",Some(&serde_json::json!({"paths":reloaded.paths,"pathCount":path_count})));
        }));
        let rejected_log=Arc::clone(&log);let rejected_state=Arc::clone(&state);let rejected_events=events.clone();
        let rejected_epoch=Arc::clone(&self.retry_epoch);let rejected_cwd=cwd.clone();let rejected_resolution=Arc::clone(&resolution);
        let rejected_path=extension_path.clone();
        let rejected=events.on(CONFIG_WATCH_REJECTED,Arc::new(move|payload|{
            let Some(rejected)=rejected_payload(payload) else { return; };
            let (path_count,error_count)=(rejected.paths.len(),rejected.errors.len());
            rejected_log(ConfigWatchLogLevel::Warn,"omo config hot-reload rejected",Some(&serde_json::json!({"paths":rejected.paths,"pathCount":path_count,"errors":rejected.errors,"errorCount":error_count})));
            // Refresh targets after rejection so a new ancestor .omo watch sees the
            // repair; never re-register synchronously (the host rejects on the same
            // stack as REGISTER -> unbounded recursion), so defer to a fresh task and
            // cap per payload fingerprint, resetting when the payload changes.
            let resolved=resolve_with_warning(&rejected_cwd,&rejected_resolution,&rejected_log,&rejected_state);
            let fingerprint=registration_json(&resolved).get("targets").map_or_else(String::new,JsonValue::to_string);
            let target_count=resolved.targets.len();
            let exhausted={
                let mut state=rejected_state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Some(registration)=&mut state.registration { registration.targets=target_specs(&resolved); }
                if fingerprint!=state.fingerprint { state.fingerprint=fingerprint; state.retries=0; }
                if state.retries>=MAX_REJECTION_RETRIES { true } else { state.retries+=1; false }
            };
            if exhausted {
                rejected_log(ConfigWatchLogLevel::Warn,"omo config hot-reload retry budget exhausted",Some(&serde_json::json!({"fingerprintTargetCount":target_count,"maxRejectionRetries":MAX_REJECTION_RETRIES})));
                return;
            }
            schedule_registration(rejected_events.clone(),rejected_path.clone(),Arc::clone(&rejected_state),Arc::clone(&rejected_epoch));
        }));
        self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).extend([ready,reloaded,rejected]);

        let shutdown_subscriptions=Arc::clone(&self.subscriptions);let shutdown_epoch=Arc::clone(&self.retry_epoch);
        api.on(EventKind::SessionShutdown,Arc::new(move|_,_|{
            shutdown_epoch.fetch_add(1,Ordering::SeqCst);
            shutdown_subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            Box::pin(async { Ok(EventResult::None) })
        }));
        // Subscribe first, then publish the initial registration (upstream order): the host may
        // reject on the same synchronous stack, so the REJECTED handler must already be live.
        // `register_config_watch` emits both the callable-free JSON payload on
        // `config-watch:register` and the typed registration carrying `validate`, so the component
        // must not emit the JSON payload itself.
        api.register_config_watch(registration);
    }
}

fn resolve_with_warning(cwd:&str,resolution:&ResolveTargetResolution,log:&ConfigWatchLogSink,state:&Arc<Mutex<Lifecycle>>)->OmoConfigWatchTargetResolution {
    let resolved=resolution(cwd);
    let warn={
        let mut guard=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if resolved.user_config_creation_discovery=="reload_required" && !guard.warned_reload_required { guard.warned_reload_required=true; true } else { false }
    };
    if warn { log(ConfigWatchLogLevel::Warn,"config-watch user config discovery requires reload",Some(&serde_json::json!({"userConfigCreationDiscovery":resolved.user_config_creation_discovery}))); }
    resolved
}

fn registration_json(resolved:&OmoConfigWatchTargetResolution)->JsonValue {
    let targets:Vec<JsonValue>=resolved.targets.iter().map(|target|serde_json::json!({"path":target.path.to_string_lossy().into_owned(),"kind":target.kind,"filterGlobs":target.filter_globs.clone()})).collect();
    serde_json::json!({"id":OMO_REGISTRATION_ID,"displayName":OMO_REGISTRATION_DISPLAY_NAME,"targets":targets})
}

fn target_specs(resolved:&OmoConfigWatchTargetResolution)->Vec<ConfigWatchTargetSpec> {
    resolved.targets.iter().map(|target|ConfigWatchTargetSpec {
        path:target.path.clone(),
        kind:if target.kind=="file" {ConfigWatchTargetKind::File} else {ConfigWatchTargetKind::Dir},
        filter_globs:target.filter_globs.clone(),
    }).collect()
}

fn registration_typed(resolved:&OmoConfigWatchTargetResolution,validator:&Arc<Mutex<OmoConfigValidator>>)->RegisteredConfigWatch {
    let validator=Arc::clone(validator);
    RegisteredConfigWatch {
        id:OMO_REGISTRATION_ID.to_owned(),
        display_name:OMO_REGISTRATION_DISPLAY_NAME.to_owned(),
        targets:target_specs(resolved),
        validate:Arc::new(move |changed:&[PathBuf]|match validator.lock().unwrap_or_else(std::sync::PoisonError::into_inner).validate(changed) {
            ConfigWatchValidation::Ok=>ApiConfigWatchValidation::Ok,
            ConfigWatchValidation::Rejected{errors}=>ApiConfigWatchValidation::Rejected{errors},
        }),
    }
}

fn schedule_registration(events:EventBus,path:String,state:Arc<Mutex<Lifecycle>>,epoch:Arc<AtomicU64>) {
    let Ok(handle)=tokio::runtime::Handle::try_current() else { return; };
    let scheduled=epoch.fetch_add(1,Ordering::SeqCst).wrapping_add(1);
    handle.spawn(async move {
        tokio::task::yield_now().await;
        if epoch.load(Ordering::SeqCst)!=scheduled { return; }
        let registration=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).registration.clone();
        if let Some(registration)=registration { events.publish_config_watch(&path,registration); }
    });
}

struct ReloadedPayload { paths:Vec<String> }
struct RejectedPayload { paths:Vec<String>, errors:Vec<String> }
fn is_record(value:&JsonValue)->bool { value.is_object() }
fn is_string_array(value:&JsonValue)->bool { value.as_array().is_some_and(|entries|entries.iter().all(JsonValue::is_string)) }
fn strings(value:&JsonValue)->Vec<String> { value.as_array().map_or_else(Vec::new,|entries|entries.iter().filter_map(JsonValue::as_str).map(str::to_owned).collect()) }
fn reloaded_payload(value:&JsonValue)->Option<ReloadedPayload> {
    if !is_record(value) || value.get("registrationId").and_then(JsonValue::as_str)!=Some(OMO_REGISTRATION_ID) || !value.get("paths").is_some_and(is_string_array) { return None; }
    Some(ReloadedPayload{paths:strings(&value["paths"])})
}
fn rejected_payload(value:&JsonValue)->Option<RejectedPayload> {
    let reloaded=reloaded_payload(value)?;
    if !value.get("errors").is_some_and(is_string_array) { return None; }
    Some(RejectedPayload{paths:reloaded.paths,errors:strings(&value["errors"])})
}
