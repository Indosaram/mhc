use maho_ext_cursor_cli_oauth::{models::{resolve_catalog,parse_models_listing},models_probe::{run_models_probe,ModelProbeError}};
use std::{collections::BTreeMap,os::unix::fs::PermissionsExt};
#[tokio::test]
async fn full_output_and_explicit_home_through_real_probe() {
    let dir=tempfile::tempdir().expect("directory");let executable=dir.path().join("cursor-agent");
    let script="#!/bin/sh\n[ \"$1\" = models ] || exit 2\n[ \"$AGENT_CLI_CREDENTIAL_STORE\" = file ] || exit 3\n[ -z \"$SENPI_PROBE_SECRET\" ] || exit 4\ni=0; while [ $i -lt 400 ]; do printf 'model-%s - Model %s\\n' \"$i\" \"$i\"; i=$((i+1)); done\n";
    std::fs::write(&executable,script).expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let stdout=dir.path().join("stdout.txt");let environment=BTreeMap::from([("SENPI_PROBE_SECRET".into(),"must-not-leak".into())]);
    let models=resolve_catalog(dir.path(),1000.0,None,||async {
        run_models_probe(&executable,&stdout,15000,dir.path().to_str().expect("home"),&environment).await?;
        Ok(std::fs::read_to_string(&stdout)?)
    }).await;
    let listing=std::fs::read_to_string(stdout).expect("listing");assert!(listing.len()>8192);assert_eq!(models.len(),400);assert_eq!(parse_models_listing(&listing).last().expect("last").id,"model-399");
}
#[tokio::test]
async fn failed_probe_returns_exit_status() {
    let dir=tempfile::tempdir().expect("directory");let executable=dir.path().join("cursor-agent");
    std::fs::write(&executable,"#!/bin/sh\nexit 7\n").expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let result=run_models_probe(&executable,&dir.path().join("stdout"),15000,dir.path().to_str().expect("home"),&BTreeMap::new()).await;
    assert!(matches!(result,Err(ModelProbeError::Exit {exit_code:Some(7),signal:None})));
}
#[tokio::test]
async fn failed_spawn_preserves_io_error_and_closes_output() {
    let dir=tempfile::tempdir().expect("directory");let stdout=dir.path().join("stdout");
    let result=run_models_probe(&dir.path().join("missing"),&stdout,15000,dir.path().to_str().expect("home"),&BTreeMap::new()).await;
    assert!(matches!(result,Err(ModelProbeError::Io(error)) if error.kind()==std::io::ErrorKind::NotFound));
    assert_eq!(std::fs::metadata(&stdout).expect("created output").len(),0);
    std::fs::remove_file(stdout).expect("output released");
}
#[tokio::test]
async fn signalled_probe_reports_signal_after_settlement() {
    let dir=tempfile::tempdir().expect("directory");let executable=dir.path().join("cursor-agent");
    std::fs::write(&executable,"#!/bin/sh\nkill -TERM $$\n").expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let result=run_models_probe(&executable,&dir.path().join("stdout"),15000,dir.path().to_str().expect("home"),&BTreeMap::new()).await;
    assert!(matches!(result,Err(ModelProbeError::Exit {exit_code:None,signal:Some(15)})));
}
#[tokio::test(start_paused = true)]
async fn probe_deadline_kills_and_settles_child() {
    let dir=tempfile::tempdir().expect("directory");let executable=dir.path().join("cursor-agent");
    std::fs::write(&executable,"#!/usr/bin/python3\nimport signal\nsignal.pause()\n").expect("script");std::fs::set_permissions(&executable,std::fs::Permissions::from_mode(0o700)).expect("permissions");
    let stdout=dir.path().join("stdout");let environment=BTreeMap::new();
    let mut probe=Box::pin(run_models_probe(&executable,&stdout,15000,dir.path().to_str().expect("home"),&environment));
    std::future::poll_fn(|cx| {assert!(probe.as_mut().poll(cx).is_pending());std::task::Poll::Ready(())}).await;
    tokio::time::advance(std::time::Duration::from_millis(15000)).await;
    assert!(matches!(probe.await,Err(ModelProbeError::Timeout {timeout_ms:15000})));
    std::fs::remove_file(stdout).expect("output released after deadline settlement");
}
