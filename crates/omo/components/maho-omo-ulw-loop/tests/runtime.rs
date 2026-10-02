use maho_omo_ulw_loop::omo_command::{run_omo_command, SpawnTarget};

#[test]
fn legacy_override_resolves_without_path_toolkit() {
    let env = std::collections::BTreeMap::from([("OMO_BIN".into(), "/custom/omo".into())]);
    assert_eq!(maho_omo_ulw_loop::omo_command::resolve_omo_bin(&env, |_| None).as_deref(), Some("/custom/omo"));
}

#[test]
fn toolkit_override_resolves_without_other_links() {
    let env = std::collections::BTreeMap::from([("OMO_AGENT_TOOLKIT_BIN".into(), "/custom/toolkit".into())]);
    assert_eq!(maho_omo_ulw_loop::omo_command::resolve_omo_bin(&env, |_| panic!("override must bypass PATH")).as_deref(), Some("/custom/toolkit"));
}

#[test]
fn empty_environment_and_path_have_no_binary() {
    assert_eq!(maho_omo_ulw_loop::omo_command::resolve_omo_bin(&Default::default(), |_| None), None);
}

#[test]
fn bare_omo_is_not_a_path_candidate() {
    assert_eq!(maho_omo_ulw_loop::omo_command::resolve_omo_bin(&Default::default(), |name| {
        if name == "omo" { Some("/stale/omo".into()) } else { None }
    }), None);
}

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
