use std::{collections::BTreeMap, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use senpi_task::runners::rpc::{model_admission::{RpcModelAdmissionOptions, create_rpc_model_admission}, process::RpcSpawnDescriptor};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let executable = args.next().ok_or("native mhc path required")?;
    let home = args.next().ok_or("isolated HOME with agent/models.json required")?;
    let descriptor = RpcSpawnDescriptor {
        command: executable,
        args: ["--offline", "--no-session", "--no-tools", "--no-skills", "--no-prompt-templates", "--list-models", "task44"].map(str::to_owned).into(),
        cwd: home.clone(),
        env: BTreeMap::from([
            ("HOME".into(), home.clone()),
            ("MAHO_CODING_AGENT_DIR".into(), std::path::Path::new(&home).join("agent").to_string_lossy().into_owned()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
    };
    let admission = create_rpc_model_admission(RpcModelAdmissionOptions {
        build_spawn: Some(Arc::new(move |_| descriptor.clone())),
        ..Default::default()
    });
    let visible = senpi_task::runners::types::RpcRunnerSpec { model: Some("task44/native".into()), ..Default::default() };
    admission(&visible).map_err(|failure| std::io::Error::other(failure.message))?;
    println!("PASS genuine native catalog admits task44/native through the default bounded process-tree probe");
    let spawns = Arc::new(AtomicUsize::new(0));
    let observed = spawns.clone();
    let runner = maho_omo_task::engine_runners::build_process_runner(senpi_task::runners::rpc_process::RpcProcessRunnerOptions {
        model_admission: Some(admission.clone()),
        spawn_child: Some(Arc::new(move |_| { observed.fetch_add(1, Ordering::SeqCst); panic!("unavailable model must fail before child spawn") })),
        ..Default::default()
    });
    let result = runner.start(&senpi_task::manager::types::ManagedStartSpec {
        task_id: "st_task44_catalog_rejected".into(), model: Some("task44/absent".into()), ..Default::default()
    });
    assert!(matches!(result, Err(senpi_task::manager::types::ManagedRunnerError::Runner(failure)) if failure.kind == senpi_task::runners::RunnerFailureKind::ModelUnavailable));
    assert_eq!(spawns.load(Ordering::SeqCst), 0);
    println!("PASS production factory rejects unavailable native model before spawn");
    let rejected_respawn = maho_omo_task::engine_runners::build_rpc_respawn_runner(senpi_task::runners::rpc_process::RpcProcessRunnerOptions {
        model_admission: Some(admission),
        spawn_child: Some(Arc::new(|_| panic!("unavailable model must fail before respawn"))),
        ..Default::default()
    }).start(&senpi_task::runners::types::RpcRunnerSpec {
        model: Some("task44/absent".into()), resume_session_path: Some("unopened-session.jsonl".into()), ..Default::default()
    });
    assert!(matches!(rejected_respawn, Err(senpi_task::manager::types::ManagedRunnerError::Runner(failure)) if failure.kind == senpi_task::runners::RunnerFailureKind::ModelUnavailable));
    println!("PASS configured native respawn preserves catalog rejection before process spawn");
    println!("OPEN provider-backed launch/resume/cancellation; no ManagedRunner fixture execution accepted");
    Ok(())
}
