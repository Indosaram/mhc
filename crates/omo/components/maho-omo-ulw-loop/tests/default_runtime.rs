mod support;

use maho_ext_api::*;
use maho_omo_ulw_loop::index::UlwLoopComponent;
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt};

#[tokio::test]
async fn path_toolkit_registration_executes_status_with_real_cwd() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let bin = root.path().join("omo-agent-toolkit");
    std::fs::write(&bin, "#!/bin/sh\nprintf '%s\\n' \"$PWD\" \"$@\" > invoked\nprintf '{\"ok\":true,\"plan\":{\"goals\":[]}}'\n")?;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700))?;
    let component = UlwLoopComponent::from_env(&BTreeMap::from([("PATH".into(), root.path().to_string_lossy().into_owned())]));
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    component.register(&mut api);
    let mut ctx = support::context(); ctx.cwd = root.path().into();
    let mut event = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup,
        initial_model_provenance: None, previous_session_file: None });
    api.registered.handlers[&EventKind::SessionStart][0](&mut event, &ctx).await?;
    assert_eq!(std::fs::read_to_string(root.path().join("invoked"))?,
        format!("{}\nulw-loop\nstatus\n--json\n", root.path().canonicalize()?.display()));
    Ok(())
}

#[tokio::test]
async fn stale_bare_omo_registration_is_inert() -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let bin = root.path().join("omo");
    std::fs::write(&bin, "#!/bin/sh\nexit 99\n")?;
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700))?;
    let component = UlwLoopComponent::from_env(&BTreeMap::from([("PATH".into(), root.path().to_string_lossy().into_owned())]));
    assert!(component.bin.is_none());
    let mut api = ExtensionApi::new(LoadedExtension::new("loop", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    component.register(&mut api);
    let mut event = ExtensionEvent::AgentEnd { messages: vec![], aborted: Some(false),
        abort_source: None, will_retry: Some(false) };
    assert!(matches!(api.registered.handlers[&EventKind::AgentEnd][0](&mut event, &support::context()).await?, EventResult::None));
    Ok(())
}
