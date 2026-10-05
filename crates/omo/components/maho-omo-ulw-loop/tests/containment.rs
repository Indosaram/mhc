use maho_omo_ulw_loop::omo_command::{SpawnTarget,run_omo_command};

#[tokio::test]
async fn large_stderr_does_not_block_stdout()->Result<(),Box<dyn std::error::Error>> {
    let root=tempfile::tempdir()?;
    let target=SpawnTarget{command:"/bin/sh".into(),args:vec!["-c".into(),"dd if=/dev/zero bs=65536 count=4 >&2 2>/dev/null; printf READY".into()]};
    let result=tokio::time::timeout(std::time::Duration::from_secs(5),run_omo_command(&target,root.path())).await?;
    assert_eq!(result.code,0);assert_eq!(result.stdout,"READY");Ok(())
}

#[tokio::test]
async fn missing_executable_returns_failure()->Result<(),Box<dyn std::error::Error>> {
    let root=tempfile::tempdir()?;
    let target=SpawnTarget{command:root.path().join("missing").to_string_lossy().into_owned(),args:vec![]};
    let result=run_omo_command(&target,root.path()).await;
    assert_eq!(result.code,1);assert!(result.stdout.is_empty());Ok(())
}

#[tokio::test]
async fn nonzero_exit_preserves_stdout()->Result<(),Box<dyn std::error::Error>> {
    let root=tempfile::tempdir()?;
    let target=SpawnTarget{command:"/bin/sh".into(),args:vec!["-c".into(),"printf partial; exit 127".into()]};
    let result=run_omo_command(&target,root.path()).await;
    assert_eq!(result.code,127);assert_eq!(result.stdout,"partial");Ok(())
}
