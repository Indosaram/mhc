use maho_ext_api::*;
use std::{collections::BTreeMap, sync::Arc, time::Duration};
struct BlockingUi {
    entered: tokio::sync::mpsc::UnboundedSender<AbortSignal>,
    calls: std::sync::atomic::AtomicUsize,
}
impl ExtensionUi for BlockingUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        options: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async move {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                return None;
            }
            let signal = options.signal.expect("reauthorization passes signal");
            self.entered.send(signal.clone()).expect("grant entry observer");
            signal.cancelled().await;
            None
        })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { true })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("custom UI must not be used".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}
#[tokio::test]
async fn real_service_reauthorization_disable_cancels_prompt_and_prevents_spawn() {
    let root = tempfile::tempdir().unwrap();
    let agent = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("mcp.json"),
        r#"{"probe":{"command":"${MISSING_TOKEN}"}}"#,
    )
    .unwrap();
    let declarations =
        maho_ext_mcp::skills::parse_skill_mcp_declarations(&[maho_ext_mcp::skills::SkillLike {
            name: "fixture".into(),
            file_path: root.path().join("SKILL.md"),
            base_dir: root.path().into(),
            scope: Some("project".into()),
            ..Default::default()
        }]);
    let registry = Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default());
    let mut service = maho_ext_mcp::service::McpService::new(registry.clone(), 73);
    service
        .attach_session(root.path(), agent.path(), &BTreeMap::new(), false, &[])
        .await
        .unwrap();
    let (entered, mut events) = tokio::sync::mpsc::unbounded_channel();
    service.set_elicitation_ui(Some(Arc::new(BlockingUi {
        entered,
        calls: Default::default(),
    })));
    // Initial denial is followed by a fresh-spawn reauthorization.
    service
        .attach_skill_mcp_servers(&declarations)
        .await
        .unwrap();
    // The cold service attach has already consumed the first materialization
    // while racing its startup connect. Reconnect itself must reauthorize.
    let connection = service.connections["probe"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    let pending = tokio::spawn({
        let connection = connection.clone();
        async move { connection.connect().await }
    });
    let signal = tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .unwrap()
        .unwrap();
    connection.disable().await.unwrap();
    // Disable must synchronously abort the same signal supplied to the UI,
    // without waiting for the pending connect task to be scheduled again.
    assert!(signal.is_aborted());
    assert!(
        tokio::time::timeout(Duration::from_secs(3), pending)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert!(connection.get_root_pid().is_none());
    service.dispose().await.unwrap();
    registry.dispose().await.unwrap();
}

#[tokio::test]
async fn service_reload_reuses_identity_but_revoke_and_edit_are_seen_before_spawn() {
    use maho_ext_mcp::{env_grants_store::*, project_skill_policy::*};
    let root = tempfile::tempdir().unwrap();
    let agent = tempfile::tempdir().unwrap();
    let source = root.path().join("mcp.json");
    std::fs::write(
        &source,
        r#"{"probe":{"command":"server","args":["${TOKEN}"]}}"#,
    )
    .unwrap();
    let skill = maho_ext_mcp::skills::SkillLike {
        name: "fixture".into(),
        file_path: root.path().join("SKILL.md"),
        base_dir: root.path().into(),
        scope: Some("project".into()),
        ..Default::default()
    };
    let registry = Arc::new(maho_ext_mcp::host_registry::HostMcpRegistry::default());
    let mut service = maho_ext_mcp::service::McpService::new(registry.clone(), 74);
    service
        .attach_session(
            root.path(),
            agent.path(),
            &BTreeMap::from([("TOKEN".into(), "secret".into())]),
            false,
            &[],
        )
        .await
        .unwrap();
    let declarations = maho_ext_mcp::skills::parse_skill_mcp_declarations(std::slice::from_ref(&skill));
    service
        .attach_skill_mcp_servers(&declarations)
        .await
        .unwrap();
    let config = service.config.as_ref().unwrap().servers["probe"]
        .config
        .as_ref()
        .unwrap();
    let authority = get_project_mcp_authority(config).unwrap();
    let connection = service.connections["probe"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    service
        .attach_skill_mcp_servers(&declarations)
        .await
        .unwrap();
    assert!(Arc::ptr_eq(
        &authority,
        &get_project_mcp_authority(
            service.config.as_ref().unwrap().servers["probe"]
                .config
                .as_ref()
                .unwrap()
        )
        .unwrap()
    ));
    assert!(Arc::ptr_eq(
        &connection,
        &service.connections["probe"].entry.lock().await.connection
    ));
    add_grant(
        &authority.store_path,
        &authority.identity,
        GrantLabel {
            project: "p".into(),
            skill: "fixture".into(),
            server: "probe".into(),
        },
        &["TOKEN".into()],
    )
    .unwrap();
    assert_eq!(
        authority.resolve().await.unwrap().config.args.unwrap(),
        ["secret"]
    );
    revoke(&authority.store_path, &authority.identity, None).unwrap();
    assert_eq!(
        authority.resolve().await.unwrap().config.args.unwrap(),
        ["${TOKEN}"]
    );
    std::fs::write(
        &source,
        r#"{"probe":{"command":"changed","args":["${TOKEN}"]}}"#,
    )
    .unwrap();
    assert!(authority.resolve().await.is_err());
    service
        .attach_skill_mcp_servers(&maho_ext_mcp::skills::parse_skill_mcp_declarations(&[
            skill,
        ]))
        .await
        .unwrap();
    assert!(!Arc::ptr_eq(
        &connection,
        &service.connections["probe"].entry.lock().await.connection
    ));
    service.dispose().await.unwrap();
    registry.dispose().await.unwrap();
}
