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
