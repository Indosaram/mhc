use std::{collections::BTreeMap, path::Path};
pub use maho_omo_memory::worker::{memory_run_supervisor, run_artifacts, run_sentinel, supervisor_process_identity, supervisor_test_signals};
#[path = "../src/worker/fixtures/mod.rs"]
pub mod fixtures;
#[path = "../src/worker/memory_run_supervisor_ic8_exit_resources.rs"]
pub mod memory_run_supervisor_ic8_exit_resources;
#[path = "../src/worker/memory_run_supervisor_ic8_process_groups.rs"]
pub mod memory_run_supervisor_ic8_process_groups;
#[path = "../src/worker/memory_run_supervisor_ic8_harness.rs"]
pub mod memory_run_supervisor_ic8_harness;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    match run().await {
        Ok(code) => std::process::exit(code),
        Err(error) => { eprintln!("{error}"); std::process::exit(1); }
    }
}

async fn run() -> Result<i32, String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env: BTreeMap<String, String> = std::env::vars().collect();
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    match args.first().map(String::as_str) {
        Some("--qa") => qa(&executable).await,
        Some("--qa-facts-model")=>{
            if args.iter().any(|arg|arg=="--list-models"){println!("fixture/facts");Ok(0)}
            else{fixtures::facts_child::run("fact",&env)}
        }
        Some("--qa-reflection-model")=>{
            if args.iter().any(|arg|arg=="--list-models"){println!("fixture/reflection");Ok(0)}
            else{fixtures::reflection_child::run("commit",&env).await}
        }
        Some("--qa-dream-model")=>{
            if args.iter().any(|arg|arg=="--list-models"){println!("fixture/reflection");Ok(0)}
            else{fixtures::dream_child::run(&env).await?;Ok(0)}
        }
        Some("--fixture-supervisor-child") => fixtures::supervisor_child::run(required(&args, 1)?, Path::new(required(&args, 2)?), &env).await,
        Some("--fixture-taskkill") => {
            #[cfg(unix)]
            fixtures::supervisor_taskkill::run(&args[1..], Path::new(env.get("OMO_MEMORY_SUPERVISOR_TASKKILL_RUN_DIR").ok_or("taskkill run directory missing")?))?;
            #[cfg(not(unix))]
            return Err("injected taskkill fixture requires Unix".into());
            Ok(0)
        }
        Some("--fixture-facts") => fixtures::facts_child::run(required(&args, 1)?, &env),
        Some("--fixture-reflection") => fixtures::reflection_child::run(required(&args, 1)?, &env).await,
        Some("--fixture-hold-lock") => { fixtures::hold_lock::run(Path::new(required(&args, 1)?)).await?; Ok(0) }
        Some("--fixture-dream") => { fixtures::dream_child::run(&env).await?; Ok(0) }
        Some("--fixture-supervisor-parent") => { fixtures::supervisor_parent::run(&executable, &[], Path::new(required(&args, 1)?)).await?; Ok(0) }
        Some("--fixture-finished-child") => { fixtures::supervisor_finished_child::run(Path::new(required(&args, 1)?)).map_err(|error|error.to_string())?; Ok(0) }
        Some("--fixture-incomplete-outcome") => fixtures::supervisor_incomplete_outcome_then_fail::run(Path::new(required(&args, 1)?)).map_err(|error|error.to_string()),
        Some("--fixture-outcome-fail") => fixtures::supervisor_outcome_then_fail::run(Path::new(required(&args, 1)?)).map_err(|error|error.to_string()),
        Some("--fixture-outcome-linger") => { fixtures::supervisor_outcome_then_linger::run(Path::new(required(&args, 1)?)).await.map_err(|error|error.to_string())?; Ok(0) }
        Some("--fixture-never-publishes") => { fixtures::supervisor_never_publishes::run(Path::new(required(&args, 1)?)).await.map_err(|error|error.to_string())?; Ok(0) }
        Some("--fixture-keepalive") => {
            let directory=Path::new(required(&args,1)?);
            fixtures::supervisor_keepalive::run(directory,&executable,&[],|operation|qa_terminal_gate(directory,operation)).await?;Ok(0)
        }
        _ => {
            maho_omo_memory::worker::memory_run_supervisor::run_entry(&args, &executable, &[], |directory, operation| {
                qa_terminal_gate(directory,operation)
            }).await?;
            Ok(0)
        }
    }
}

fn qa_terminal_gate(directory:&Path,operation:&mut dyn FnMut()->Result<(),String>)->Result<(),String>{
    let record=memory_core::locks::create_lock_record("reflection-finalize",Default::default()).map_err(|error|error.to_string())?;
    memory_core::locks::with_lock(&directory.join("terminalization.lock"),&record,&memory_core::locks::AcquireLockOptions{wait_timeout_ms:Some(2000),..Default::default()},operation).map_err(|error|error.to_string())
}

fn required(args: &[String], index: usize) -> Result<&str, String> {
    args.get(index).map(String::as_str).ok_or_else(|| format!("missing argument {index}"))
}

async fn qa(executable: &Path) -> Result<i32, String> {
    qa_facts(executable).await?;
    qa_reflection(executable,false).await?;
    qa_reflection(executable,true).await?;
    for platform in memory_run_supervisor_ic8_harness::IC8_PLATFORMS {
        for mode in ["graceful", "stubborn"] {
            let mut harness = memory_run_supervisor_ic8_harness::Ic8Harness::new(executable.into(), vec![]);
            let result = async {
                let mut run = harness.make_run(mode).await?;
                let mut supervisor = harness.launch_supervisor(&run, platform)?;
                let pid = supervisor.id().ok_or("supervisor pid missing")?;
                harness.wait_for_path(&run.root.join("child-started.json")).await?;
                (&mut run.exit.exit_socket_accepted).await.map_err(|error| error.to_string())?;
                harness.advance_clock(&run, 2000.0)?;
                harness.wait_for_path(&run.root.join("child-terminated.json")).await?;
                if mode == "stubborn" { harness.advance_clock(&run, 3000.0)?; }
                let status = harness.wait_for_exit(&mut supervisor).await?;
                if !status.success() { return Err(format!("{platform}/{mode}: supervisor exited {status}")); }
                run.exit.child_exited.await.map_err(|error| error.to_string())??;
                let outcome: run_artifacts::RunOutcome = run_artifacts::read_run_json(&run.root.join("outcome.json")).map_err(|error| error.to_string())?;
                if !outcome.timed_out || run.root.join("launch.json").exists() { return Err(format!("{platform}/{mode}: incomplete terminal outcome")); }
                harness.untrack_process_group(pid);
                Ok::<_, String>(())
            }.await;
            let cleanup = harness.cleanup().await;
            result?;
            cleanup?;
            println!("PASS supervisor {platform}/{mode}; resources cleaned");
        }
    }
    Ok(0)
}

async fn qa_facts(executable:&Path)->Result<(),String>{
    use maho_omo_memory::{facts_wiring::FactsExtractorPort,facts_runner::{FactsExtractorRunner,NativeFactsAttemptOptions}};
    use memory_core::journal::entries::{TranscriptEntry,TextTranscriptEntry};
    let root=tempfile::tempdir().map_err(|error|error.to_string())?;
    let identity=memory_core::identity::resolve::MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
    let runner=std::sync::Arc::new(FactsExtractorRunner::new(identity,std::sync::Arc::new(||0)));
    runner.queue.enqueue(memory_core::facts::queue::FactsEnqueueRequest{identity:"agent".into(),session_id:"session".into(),conversation_id:"conversation".into(),signal:None,entries:vec![TranscriptEntry::Text(TextTranscriptEntry::new("user","Question","1970-01-01T00:00:00.000Z","m:user","m")),TranscriptEntry::Text(TextTranscriptEntry::new("assistant","Answer","1970-01-01T00:00:00.000Z","m:assistant","m"))]}).map_err(|error|error.to_string())?;
    let warnings=std::sync::Arc::new(std::sync::Mutex::new(vec![]));let seen=warnings.clone();
    let mut port=runner.clone().native_extractor(NativeFactsAttemptOptions{
        resolution:maho_omo_memory::worker::resolve_model::ReflectionModelResolution::Resolved{category:"quick".into(),model:"fixture/facts".into(),thinking:None,source:None,fallbacks:vec![]},env:std::env::vars().collect(),config_sources:vec![],launch:maho_omo_memory::worker::model_preflight::Launcher{command:executable.to_string_lossy().into_owned(),prefix_args:vec!["--qa-facts-model".into()]},supervisor_command:executable.into(),supervisor_args:vec![],deadline_ms:60_000,termination_grace_ms:100,max_output_bytes:4096,people:memory_core::facts::person_routing::FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200},sandbox:None,warn:std::sync::Arc::new(move|error|{seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(error.to_owned());}),
    });
    tokio::time::timeout(std::time::Duration::from_secs(15),port.launch_pending(None)).await.map_err(|error|error.to_string())??;
    if !runner.queue.list_pending().map_err(|error|error.to_string())?.is_empty(){return Err("facts QA queue was not consumed".into());}
    let runs=std::fs::read_dir(runner.identity.paths.facts.join("runs")).map_err(|error|error.to_string())?.collect::<Result<Vec<_>,_>>().map_err(|error|error.to_string())?;
    if runs.len()!=1{return Err(format!("facts QA expected one run, got {}",runs.len()));}
    let dir=runs[0].path();let record:maho_omo_memory::facts_runner_types::FactsFinalRecord=run_artifacts::read_run_json(&dir.join("final.json")).map_err(|error|error.to_string())?;
    if record.outcome!=maho_omo_memory::facts_runner_types::FactsTerminalOutcome::Committed||record.sha.is_none()||dir.join("launch.json").exists()||dir.join("facts-payload.json").exists(){return Err("facts QA incomplete terminal commit or cleanup".into());}
    let warnings=warnings.lock().map_err(|error|error.to_string())?;if !warnings.is_empty(){return Err(format!("facts QA warnings: {warnings:?}"));}
    println!("PASS native facts queue/model/supervisor/apply; resources cleaned");Ok(())
}

async fn qa_reflection(executable:&Path,dream:bool)->Result<(),String>{
    use maho_omo_memory::worker::{runner::{SenpiSubprocessRunner,ReflectionRunnerInput,NativeReflectionExecutionOptions,NativeReflectionCallbacks},resolve_model::ReflectionModelResolution,model_preflight::{ModelPreflight,Launcher},spawn_supervisor::ReflectionChildOptions};
    let root=tempfile::tempdir().map_err(|error|error.to_string())?;
    let context=maho_omo_memory::context::MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root.path(),"agent"),maho_omo_memory::binding::create_memory_binding("agent","repo",0.0));
    let settings=maho_omo_memory::reflection_settings::resolve_memory_settings(None)?;
    let store=maho_omo_memory::identity_runtime::create_identity_reservation_store(&context,&settings)?;
    let run=store.try_reserve(memory_core::reflection::ReflectionRequest{trigger:if dream{memory_core::reflection::ReflectionTrigger::Dream}else{memory_core::reflection::ReflectionTrigger::Manual},origin:dream.then_some(memory_core::reflection::DreamOrigin::Manual),conversation_ids:vec![],snapshots:vec![],focus:None,recent_n:None,target_doc:dream.then(||"system/persona.md".into())}).map_err(|error|error.to_string())?.run;
    let identity=maho_omo_memory::identity_runtime::as_memory_identity(&context);
    let config=serde_json::json!({"memory":settings});let mut env:std::collections::BTreeMap<String,String>=std::env::vars().collect();if dream{env.insert("DREAM_FIXTURE_EMPTY_LEDGERS".into(),"1".into());}let launcher=Launcher{command:executable.to_string_lossy().into_owned(),prefix_args:vec![if dream{"--qa-dream-model"}else{"--qa-reflection-model"}.into()]};
    let now=||chrono::Utc::now().timestamp_millis();let started=chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    let resolution=ReflectionModelResolution::Resolved{category:"quick".into(),model:"fixture/reflection".into(),thinking:None,source:None,fallbacks:vec![]};
    let result=tokio::time::timeout(std::time::Duration::from_secs(15),SenpiSubprocessRunner::default().launch_native(
        ReflectionRunnerInput{run:&run,identity:&identity,config:&config,resolution:&resolution,reservation:store.as_ref(),started_at:&started,now_ms:&now},&mut ModelPreflight::default(),
        NativeReflectionExecutionOptions{env:&env,sources:&[],launcher:&launcher,parent_session_file:None,parent_cwd:None,route:None,sandbox:None,deadline_ms:Some(60_000),child:ReflectionChildOptions{termination_grace_ms:100.0,max_output_bytes:4096,supervisor_command:executable,supervisor_args:&[],launched_at:now()},callbacks:NativeReflectionCallbacks{ensure_renderer:&||{},health_alert:&|_|{},append_launched:&||Ok(()),warn:&|error|eprintln!("{error}")}},None,|operation|{
            let record=memory_core::locks::create_lock_record("reflection-finalize",memory_core::locks::CreateLockRecordOptions{run_id:Some(run.run_id.clone())}).map_err(|error|error.to_string())?;
            memory_core::locks::with_lock(&identity.paths.reflection.join("runs").join(&run.run_id).join("terminalization.lock"),&record,&memory_core::locks::AcquireLockOptions{wait_timeout_ms:Some(2000),..Default::default()},operation).map_err(|error|error.to_string())
        }
    )).await.map_err(|error|error.to_string())??;
    if result.outcome!="merged"{return Err(format!("reflection QA failed: {} {:?}",result.outcome,result.detail));}
    if dream{if !std::fs::read_to_string(identity.paths.repo.join("system/persona.md")).map_err(|error|error.to_string())?.contains("Maintained by dream."){return Err("dream QA target was not integrated".into());}}
    else if !identity.paths.repo.join("system/reflected.md").exists(){return Err("reflection QA document missing".into());}
    if store.read_state().map_err(|error|error.to_string())?.active.is_some(){return Err("reflection QA active reservation retained".into());}
    let dir=identity.paths.reflection.join("runs").join(&run.run_id);if !dir.join("final.json").exists()||dir.join("launch.json").exists(){return Err("reflection QA incomplete terminal cleanup".into());}
    println!("PASS native {} reservation/model/supervisor/merge; resources cleaned",if dream{"dream"}else{"reflection"});Ok(())
}
