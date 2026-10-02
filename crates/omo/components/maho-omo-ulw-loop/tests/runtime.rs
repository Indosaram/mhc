use maho_omo_ulw_loop::omo_command::{run_omo_command, SpawnTarget};

#[tokio::test]
async fn real_process_receives_status_arguments_and_cwd() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let target = SpawnTarget { command: "/bin/sh".into(), args: vec!["-c".into(),
        "printf '%s\\n' \"$PWD\" \"$@\"".into(), "toolkit".into(), "ulw-loop".into(), "status".into(), "--json".into()] };
    let result = run_omo_command(&target, root.path()).await;
    assert_eq!(result.code, 0);
    assert_eq!(result.stdout, format!("{}\nulw-loop\nstatus\n--json\n", root.path().canonicalize()?.display()));
    Ok(())
}
