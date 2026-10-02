use std::{collections::{BTreeMap,BTreeSet},rc::Rc,sync::{Arc,atomic::{AtomicBool,Ordering}}};
use config_migration::{ConfigMigrationDiscoveryOptions,CreateLegacyConfigMigrationPlansOptions,PathOperations,create_legacy_config_migration_plans};
use omo_config_core::*;
use maho_ext_api::{Extension,ExtensionApi,EventKind,EventResult,NotificationType};

#[derive(Default)]
pub struct SenpiStartupMigrationResult { pub error:Option<String>,pub journal_resumed:bool,pub migrated_from:Vec<String>,pub results:Vec<MigrationRunResult> }
#[derive(Default)]
pub struct SenpiStartupMigrationOptions<'a> {
    pub backup_timestamp:Option<&'a str>,pub clock:Option<&'a dyn MigrationClock>,
    pub discovery_file_system:Option<&'a dyn config_migration::ConfigMigrationDiscoveryFileSystem>,
    pub file_system:Option<&'a dyn MigrationFileSystem>,pub pid:Option<u32>,
    pub is_process_alive:Option<Box<dyn Fn(u32)->bool+'a>>,pub on_boundary:Option<Box<MigrationBoundaryHook<'a>>>,
    pub platform:Option<config_migration::Platform>,
}

pub fn run_senpi_startup_migration(cwd:&str,environment:&BTreeMap<String,String>,home_dir:&str) -> SenpiStartupMigrationResult {
    run_senpi_startup_migration_with_options(cwd,environment,home_dir,SenpiStartupMigrationOptions::default())
}
pub fn run_senpi_startup_migration_with_options<'a>(cwd:&str,environment:&BTreeMap<String,String>,home_dir:&str,options:SenpiStartupMigrationOptions<'a>) -> SenpiStartupMigrationResult {
    if home_dir.is_empty() { return SenpiStartupMigrationResult{error:Some("Cannot migrate configuration because no home directory is available".into()),..Default::default()}; }
    let plans=match create_legacy_config_migration_plans(&CreateLegacyConfigMigrationPlansOptions{backup_timestamp:options.backup_timestamp,discovery:ConfigMigrationDiscoveryOptions{cwd,environment,file_system:options.discovery_file_system,home_dir,path_operations:if options.platform==Some(config_migration::Platform::Win32){PathOperations::Win32}else{PathOperations::Posix},platform:options.platform,tauri_config_dirs:None}}) {
        Ok(plans)=>plans,Err(error)=>return SenpiStartupMigrationResult{error:Some(error.to_string()),..Default::default()},
    };
    let batch=run_migrations(RunMigrationsOptions{
        after_migrations:None,clock:options.clock,discover:Box::new(move ||plans.iter().map(|plan| {
            let transform=Rc::clone(&plan.transform);
            MigrationPlan{id:plan.id.clone(),mode:plan.mode,sources:plan.sources.clone(),target_path:plan.target_path.clone(),transform:Box::new(move |loaded| { let result=transform(loaded)?; Ok(MigrationTransformResult{diagnostics:result.diagnostics,document:serde_json::Value::Object(result.document)}) })}
        }).collect()),dry_run:false,env:Some(environment.clone()),file_system:options.file_system,is_process_alive:options.is_process_alive,lease_duration_ms:None,on_boundary:options.on_boundary,pid:options.pid,write_target:None,
    });
    match batch {
        Err(error)=>SenpiStartupMigrationResult{error:Some(error.to_string()),..Default::default()},
        Ok(batch)=>{
            let migrated_from:BTreeSet<_>=batch.results.iter().filter(|r|r.status==MigrationStatus::Migrated).flat_map(|r|r.preview.iter().flat_map(|p|p.backup_moves.iter().map(|m|m.from.clone()))).collect();
            SenpiStartupMigrationResult{error:(batch.status==MigrationBatchStatus::Locked).then(||"Configuration migration is already running".into()),journal_resumed:batch.journal_resumed,migrated_from:migrated_from.into_iter().collect(),results:batch.results}
        }
    }
}

pub type StartupMigrationRunner=Arc<dyn Fn(&str)->SenpiStartupMigrationResult+Send+Sync>;
pub type StartupConfigLoader=Arc<dyn Fn(&str)->maho_omo_config_resolution::SenpiOmoConfigResult+Send+Sync>;
#[derive(Default)]
pub struct ConfigStartupComponent { pub run_migration:Option<StartupMigrationRunner>,pub load_config:Option<StartupConfigLoader> }
impl Extension for ConfigStartupComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let cwd=api.cwd.to_string_lossy().into_owned(); let env:BTreeMap<_,_>=std::env::vars().collect();
        let home=env.get("HOME").or_else(||env.get("USERPROFILE")).map(String::as_str).unwrap_or_default();
        let migration=self.run_migration.as_ref().map_or_else(||run_senpi_startup_migration(&cwd,&env,home),|run|run(&cwd));
        let config=self.load_config.as_ref().map_or_else(||maho_omo_config_resolution::load_senpi_omo_config(LoadOmoConfigOptions{cwd:Some(cwd.clone()),env:Some(env),..Default::default()}),|load|load(&cwd));
        let mut notices=Vec::new();
        if let Some(error)=migration.error { notices.push((format!("omo-senpi: configuration migration: {error}"),NotificationType::Warning)); }
        else if !migration.migrated_from.is_empty() { notices.push((format!("omo-senpi: migrated legacy configuration from {}",migration.migrated_from.join(", ")),NotificationType::Info)); }
        else if migration.journal_resumed { notices.push(("omo-senpi: recovered an interrupted configuration migration".into(),NotificationType::Info)); }
        let diagnostics:Vec<_>=migration.results.iter().flat_map(|r|r.diagnostics.iter().map(String::as_str)).collect();
        if !diagnostics.is_empty() { notices.push((format!("omo-senpi: configuration migration: {}",diagnostics.join("; ")),NotificationType::Warning)); }
        let diagnostics:Vec<_>=config.diagnostics.iter().map(|d|match d { maho_omo_config_resolution::SenpiConfigDiagnostic::Config(d)=>d.message.as_str(),maho_omo_config_resolution::SenpiConfigDiagnostic::Model(d)=>d.message.as_str() }).collect();
        if !diagnostics.is_empty() { notices.push((format!("omo-senpi: configuration diagnostics: {}",diagnostics.join("; ")),NotificationType::Warning)); }
        let reported=Arc::new(AtomicBool::new(false));
        api.on(EventKind::SessionStart,Arc::new(move |_,ctx| { let reported=Arc::clone(&reported); let notices=notices.clone(); Box::pin(async move {
            if !notices.is_empty() && !reported.swap(true,Ordering::SeqCst) { for (message,kind) in notices { if ctx.has_ui { ctx.ui.notify(&message,kind); } else { eprintln!("{message}"); } } }
            Ok(EventResult::None)
        }) }));
    }
}
