use std::{io::Write,os::unix::fs::OpenOptionsExt,path::Path};
pub fn claim_notice(state_dir:&Path)->std::io::Result<bool> {
    std::fs::create_dir_all(state_dir)?;
    match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(state_dir.join("notice-shown")) {
        Ok(mut file)=>{file.write_all(b"shown\n")?;Ok(true)},
        Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>Ok(false),
        Err(error)=>Err(error),
    }
}
pub fn register_omo_native_notice(api:&mut maho_ext_api::ExtensionApi,env:telemetry_core::TelemetryEnv,state_dir:std::path::PathBuf,enabled:crate::omo_native_component::ConfigEnabled) {
    use std::sync::{Arc,atomic::{AtomicBool,Ordering}};
    let reported=Arc::new(AtomicBool::new(false));
    api.on(maho_ext_api::EventKind::SessionStart,Arc::new(move |_,ctx| {
        let product=crate::product_identity::create_omo_native_product_config();
        if enabled(&ctx.cwd) && telemetry_core::is_telemetry_client_enabled(&telemetry_core::TelemetryClientEnabledInput::for_product(Some(&env),&product)) {
            match claim_notice(&state_dir) {Ok(true)=>ctx.ui.notify("omo-senpi sends anonymous usage telemetry (no prompts, no paths). Docs: https://github.com/code-yeongyu/oh-my-openagent/blob/dev/docs/reference/senpi-telemetry.md - opt out: DO_NOT_TRACK=1",maho_ext_api::NotificationType::Info),Ok(false)=>{},Err(error)=>{if !reported.swap(true,Ordering::SeqCst) {eprintln!("telemetry_capture_failed: omo-native-notice: {error}");}}}
        }
        Box::pin(async {Ok(maho_ext_api::EventResult::None)})
    }));
}
#[cfg(test)]
mod tests {
    use super::*;
    fn start()->maho_ext_api::ExtensionEvent {maho_ext_api::ExtensionEvent::SessionStart(maho_ext_api::SessionStartEvent {reason:maho_ext_api::SessionReason::Startup,initial_model_provenance:None,previous_session_file:None})}
    #[tokio::test] async fn registered_notice_once() {use crate::telemetry_test_support::*;let t=tempfile::tempdir().unwrap();let mut api=api();register_omo_native_notice(&mut api,env(t.path()),t.path().join("notice"),std::sync::Arc::new(|_|true));let (ctx,messages)=context_with_notifications(t.path(),"s");dispatch(&api,start(),&ctx).await;dispatch(&api,start(),&ctx).await;assert_eq!(messages.lock().unwrap().len(),1);}
    #[tokio::test] async fn optout_no_notice_or_marker() {use crate::telemetry_test_support::*;let t=tempfile::tempdir().unwrap();let mut api=api();let mut env=env(t.path());env.insert("DO_NOT_TRACK".into(),"1".into());register_omo_native_notice(&mut api,env,t.path().join("notice"),std::sync::Arc::new(|_|true));let (ctx,messages)=context_with_notifications(t.path(),"s");dispatch(&api,start(),&ctx).await;assert!(messages.lock().unwrap().is_empty());assert!(!t.path().join("notice").exists());}
    #[tokio::test] async fn disabled_config_no_notice_or_marker() {use crate::telemetry_test_support::*;let t=tempfile::tempdir().unwrap();let mut api=api();register_omo_native_notice(&mut api,env(t.path()),t.path().join("notice"),std::sync::Arc::new(|_|false));let (ctx,messages)=context_with_notifications(t.path(),"s");dispatch(&api,start(),&ctx).await;assert!(messages.lock().unwrap().is_empty());assert!(!t.path().join("notice").exists());}
    #[test] fn no_audit_commands() {use crate::telemetry_test_support::*;let t=tempfile::tempdir().unwrap();let mut api=api();register_omo_native_notice(&mut api,env(t.path()),t.path().join("notice"),std::sync::Arc::new(|_|true));assert!(api.registered.commands.is_empty());}
    #[test] fn once_per_machine() {let t=tempfile::tempdir().unwrap();assert!(claim_notice(t.path()).unwrap());assert!(!claim_notice(t.path()).unwrap());}
    #[test] fn private_marker() {use std::os::unix::fs::PermissionsExt;let t=tempfile::tempdir().unwrap();claim_notice(t.path()).unwrap();assert_eq!(std::fs::metadata(t.path().join("notice-shown")).unwrap().permissions().mode()&0o777,0o600);}
    #[test] fn blocked_directory_reports_error() {let t=tempfile::tempdir().unwrap();let file=t.path().join("blocked");std::fs::write(&file,"").unwrap();assert!(claim_notice(&file).is_err());}
    #[test] fn stale_marker_suppresses() {let t=tempfile::tempdir().unwrap();std::fs::write(t.path().join("notice-shown"),"shown\n").unwrap();assert!(!claim_notice(t.path()).unwrap());}
    #[test] fn simultaneous_claim_only_one() {let t=tempfile::tempdir().unwrap();let barrier=std::sync::Arc::new(std::sync::Barrier::new(2));let results=std::thread::scope(|scope|{let mut workers=Vec::new();for _ in 0..2 {let barrier=std::sync::Arc::clone(&barrier);let path=t.path();workers.push(scope.spawn(move ||{barrier.wait();claim_notice(path).unwrap()}));}workers.into_iter().map(|w|w.join().unwrap()).collect::<Vec<_>>()});assert_eq!(results.into_iter().filter(|claimed|*claimed).count(),1);}
}
