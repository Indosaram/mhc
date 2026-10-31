use maho_ext_mcp::{config_schema::*, connection::*, host_registry::*, log::McpLogger};
use std::sync::{Arc, Mutex};
#[tokio::test]
async fn sharing_preserves_the_connection_until_the_last_owner_detaches() {
    let root = tempfile::tempdir().unwrap();
    let registry = HostMcpRegistry::default();
    let first = registry.attach(
        "key",
        1,
        || {
            ServerConnection::new(
                "server",
                McpServerConfig {
                    enabled: Some(true),
                    ..Default::default()
                },
                None,
                Arc::new(Mutex::new(
                    McpLogger::new("server", root.path(), None).unwrap(),
                )),
            )
        },
        true,
    );
    let second = registry.attach(
        "key",
        2,
        || panic!("shared entry should avoid factory"),
        true,
    );
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(registry.size(), 1);
    registry.detach("key", 1).await.unwrap();
    assert_eq!(first.state(), ServerConnectionState::Idle);
    registry.detach("key", 2).await.unwrap();
    assert_eq!(first.state(), ServerConnectionState::Disabled);
    assert_eq!(registry.size(), 0);
}
#[tokio::test]
async fn duplicate_owner_attach_is_reference_counted() {
    let root = tempfile::tempdir().unwrap();
    let registry = HostMcpRegistry::default();
    registry.attach(
        "key",
        1,
        || {
            ServerConnection::new(
                "server",
                McpServerConfig::default(),
                None,
                Arc::new(Mutex::new(
                    McpLogger::new("server", root.path(), None).unwrap(),
                )),
            )
        },
        false,
    );
    registry.attach(
        "key",
        1,
        || panic!("owned entry should avoid factory"),
        false,
    );
    registry.detach("key", 1).await.unwrap();
    assert_eq!(registry.size(), 1);
    assert_eq!(registry.for_each_owner("key"), vec![1]);
    registry.detach("key", 1).await.unwrap();
    assert_eq!(registry.size(), 0);
    assert!(matches!(
        registry.detach("key", 1).await,
        Err(RegistryDetachError::Owner(HostMcpRegistryError {
            code: "unknown_owner",
            ..
        }))
    ));
}
async fn project_config(root: &std::path::Path, server: &str) -> McpServerConfig {
    let skill = root.join("SKILL.md");
    std::fs::write(
        &skill,
        format!("---\nname: fixture\nmcp:\n  {server}:\n    command: synthetic-command\n---\n"),
    )
    .unwrap();
    let declarations =
        maho_ext_mcp::skills::parse_skill_mcp_declarations(&[maho_ext_mcp::skills::SkillLike {
            name: "fixture".into(),
            file_path: skill,
            base_dir: root.into(),
            scope: Some("project".into()),
            ..Default::default()
        }]);
    maho_ext_mcp::project_skill_policy::prepare_project_skill_server(
        server,
        &declarations.servers[server],
        root,
        root,
        true,
        &Default::default(),
        None,
    )
    .await
    .unwrap()
    .config
    .unwrap()
}
// host-registry.ts invalidateIdentity (#3796): only connections admitted by that identity renew.
#[tokio::test]
async fn invalidate_identity_bumps_only_connections_of_that_identity() {
    let first_root = tempfile::tempdir().unwrap();
    let other_root = tempfile::tempdir().unwrap();
    let first_config = project_config(first_root.path(), "probe").await;
    let other_config = project_config(other_root.path(), "probe").await;
    let identity = maho_ext_mcp::project_skill_policy::get_project_mcp_authority(&first_config)
        .unwrap()
        .identity
        .clone();
    let registry = HostMcpRegistry::default();
    let logger = || {
        Arc::new(Mutex::new(
            McpLogger::new("probe", first_root.path(), None).unwrap(),
        ))
    };
    let first = registry.attach(
        "first",
        1,
        || ServerConnection::new("probe", first_config.clone(), None, logger()),
        false,
    );
    let other = registry.attach(
        "other",
        1,
        || ServerConnection::new("probe", other_config.clone(), None, logger()),
        false,
    );
    let plain = registry.attach(
        "plain",
        1,
        || {
            ServerConnection::new(
                "probe",
                McpServerConfig {
                    enabled: Some(true),
                    ..Default::default()
                },
                None,
                logger(),
            )
        },
        false,
    );
    assert_eq!(
        first.declaration_identity().as_deref(),
        Some(identity.as_str())
    );
    assert_ne!(
        other.declaration_identity().as_deref(),
        Some(identity.as_str())
    );
    assert_eq!(plain.declaration_identity(), None);
    registry.invalidate_identity(&identity).await.unwrap();
    assert_eq!(first.generation(), 1);
    assert_eq!(other.generation(), 0);
    assert_eq!(plain.generation(), 0);
    registry.dispose().await.unwrap();
}
