use std::{collections::BTreeMap,sync::Arc};
use maho_ext_api::{ExtensionApi,EventKind,EventResult};
pub const SENPI_RPC_CHILD_MARKER_ENV:&str="SENPI_CODING_AGENT_SESSION_DIR";
pub type FamilySweep=Arc<dyn Fn()->Result<(),String>+Send+Sync>;
pub type SweepLogger=Arc<dyn Fn(&str)+Send+Sync>;
pub struct SessionStartProcessSweepOptions { pub env:BTreeMap<String,String>,pub sweep:FamilySweep,pub info:SweepLogger,pub warn:SweepLogger }
pub fn sweep_omo_families(current_version:&str,warn:&(dyn Fn(&str)+Sync)) {
    use utils::process_sweep::{sweep_orphaned_lsp_daemon_proxies,sweep_stale_lsp_daemon_versions,SweepOrphanedLspDaemonProxiesOptions,SweepStaleLspDaemonVersionsOptions,ProcessFamilySweepOptions};
    std::thread::scope(|scope| {
        scope.spawn(|| sweep_orphaned_lsp_daemon_proxies(&SweepOrphanedLspDaemonProxiesOptions { sweep:ProcessFamilySweepOptions { log:Some(warn),..Default::default() },..Default::default() }));
        sweep_stale_lsp_daemon_versions(&SweepStaleLspDaemonVersionsOptions { current_version:Some(current_version.into()),sweep:ProcessFamilySweepOptions { log:Some(warn),..Default::default() },..Default::default() });
    });
}
pub fn run_session_start_process_sweep(options:&SessionStartProcessSweepOptions) {
    if options.env.contains_key(SENPI_RPC_CHILD_MARKER_ENV) { (options.info)("omo-senpi process sweep skipped: running inside a senpi-task RPC child"); return; }
    if let Err(error)=(options.sweep)() { (options.warn)(&format!("omo-senpi process sweep failed: {error}")); }
}
pub fn wire_session_start_process_sweep(api:&mut ExtensionApi,options:SessionStartProcessSweepOptions) {
    let options=Arc::new(options);
    api.on(EventKind::SessionStart,Arc::new(move |_,_| { let options=options.clone(); Box::pin(async move { std::thread::spawn(move || run_session_start_process_sweep(&options)); Ok(EventResult::None) }) }));
}
