use std::io::{self,Write};
#[test] fn claim_child()->io::Result<()> {
 let Some(path)=std::env::var_os("MAHO_ONBOARDING_RACE_DIR") else {return Ok(());};
 println!("READY");io::stdout().flush()?;let mut release=String::new();io::stdin().read_line(&mut release)?;
 std::process::exit(if maho_omo_onboarding::state::claim_onboarding(std::path::Path::new(&path)){0}else{1});
}
async fn child(path:&std::path::Path)->io::Result<tokio::process::Child> {
 use tokio::io::{AsyncBufReadExt,BufReader};
 let mut child=tokio::process::Command::new(std::env::current_exe()?).args(["--exact","claim_child","--nocapture"]).env("MAHO_ONBOARDING_RACE_DIR",path).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn()?;
 let stdout=child.stdout.take().ok_or_else(||io::Error::other("stdout missing"))?;let mut lines=BufReader::new(stdout).lines();
 tokio::time::timeout(std::time::Duration::from_secs(5),async {while let Some(line)=lines.next_line().await? {if line=="READY" {return Ok(());}}Err(io::Error::other("child exited before ready"))}).await.map_err(io::Error::other)??;
 Ok(child)
}
#[tokio::test] async fn cross_process_claim_has_one_winner()->io::Result<()> {
 use tokio::io::AsyncWriteExt;
 let root=tempfile::tempdir()?;let mut a=child(root.path()).await?;let mut b=child(root.path()).await?;
 for child in [&mut a,&mut b] {child.stdin.take().ok_or_else(||io::Error::other("stdin missing"))?.write_all(b"release\n").await?;}
 let(a,b)=tokio::time::timeout(std::time::Duration::from_secs(10),async {tokio::try_join!(a.wait(),b.wait())}).await.map_err(io::Error::other)??;
 assert_ne!(a.success(),b.success());Ok(())
}
#[tokio::test] async fn waiting_child_cleanup_observes_exit()->io::Result<()> {let root=tempfile::tempdir()?;let mut child=child(root.path()).await?;child.kill().await?;assert!(!child.wait().await?.success());Ok(())}
