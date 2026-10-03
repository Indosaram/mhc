use maho_codemode::kernels::py::process::*;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

#[test]
fn command_split_preserves_literal_space_semantics() {
    assert_eq!(split_command("  python3  -u ").unwrap(),("python3".into(),vec!["-u".into()]));
    assert!(split_command("   ").is_err());
    assert_eq!(split_command("python\t-u").unwrap().0,"python\t-u");
}

#[tokio::test]
async fn spawn_names_parent_and_hard_kill_reaps_group_leader() {
    let options=KernelSpawnOptions {command:"python3".into(),args:vec!["-u".into(),"-c".into(),"import os,time; print(os.environ['SENPI_PY_KERNEL_PARENT_PID'],flush=True); time.sleep(1000)".into()],cwd:"/tmp".into(),env:std::env::vars().collect()};
    let mut child=default_spawn(&options).unwrap();
    let pid=child.id().unwrap();
    let mut lines=BufReader::new(child.stdout.take().unwrap()).lines();
    let parent=tokio::time::timeout(Duration::from_secs(2),lines.next_line()).await.unwrap().unwrap().unwrap();
    assert_eq!(parent,std::process::id().to_string());
    hard_kill(&mut child,Duration::from_secs(2)).await.unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{pid}")).exists());
}

#[tokio::test(start_paused = true)]
async fn timeout_bounds_unresolved_future() {
    assert_eq!(with_timeout(std::future::pending::<()>(),Duration::from_secs(1),"expired").await,Err("expired".into()));
}
