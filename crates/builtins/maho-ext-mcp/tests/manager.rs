//! Ports of senpi `test/suite/regressions/2716-mcp-manager{,-write-gate,-dispatch,-auth}.test.ts`
//! at the `manage` boundary: only user choices are scripted; the manager builds every menu from a
//! real attached `McpService` and real config files.
use std::{
    collections::VecDeque,
    path::Path,
    sync::{Arc, Mutex},
};

use maho_ext_mcp::{
    commands::{McpManagerCommand, McpNotify},
    host_registry::HostMcpRegistry,
    manager::{ManagerReattach, ManagerRun, McpManagerAuthAction, manage},
    manager_view::{BoxFuture, McpManagerMenu, McpManagerSurface, MenuBuild, MenuSubscribe},
    service::McpService,
};

struct ScriptedSurface {
    choices: Mutex<VecDeque<Option<String>>>,
    menus: Mutex<Vec<McpManagerMenu>>,
}

impl ScriptedSurface {
    fn new(choices: &[Option<&str>]) -> Self {
        Self {
            choices: Mutex::new(choices.iter().map(|c| c.map(str::to_owned)).collect()),
            menus: Mutex::new(Vec::new()),
        }
    }
    fn menus(&self) -> Vec<McpManagerMenu> {
        self.menus.lock().unwrap().clone()
    }
    fn values(&self, index: usize) -> Vec<String> {
        self.menus()[index]
            .items
            .iter()
            .map(|item| item.value.clone())
            .collect()
    }
}

impl McpManagerSurface for ScriptedSurface {
    fn menu(
        &self,
        build: MenuBuild,
        _subscribe: Option<MenuSubscribe>,
    ) -> BoxFuture<'_, Result<Option<String>, String>> {
        Box::pin(async move {
            let menu = build().await?;
            self.menus.lock().unwrap().push(menu.clone());
            let choice = self.choices.lock().unwrap().pop_front().flatten();
            if let Some(choice) = &choice {
                assert!(
                    menu.items.iter().any(|item| &item.value == choice),
                    "menu {:?} did not offer {choice:?}",
                    menu.title
                );
            }
            Ok(choice)
        })
    }
    fn status(&self, _: &str, _: &str) {}
    fn columns(&self) -> usize {
        120
    }
}

async fn attach(
    cwd: &Path,
    agent_dir: &Path,
    trusted: bool,
    global: serde_json::Value,
) -> Arc<tokio::sync::Mutex<McpService>> {
    std::fs::write(
        agent_dir.join("mcp.json"),
        serde_json::json!({ "mcpServers": global }).to_string(),
    )
    .unwrap();
    let mut service = McpService::new(Arc::new(HostMcpRegistry::default()), 1);
    service
        .attach_session(cwd, agent_dir, &Default::default(), trusted, &[])
        .await
        .unwrap();
    Arc::new(tokio::sync::Mutex::new(service))
}

fn write_project(cwd: &Path, servers: serde_json::Value) -> std::path::PathBuf {
    let path = cwd.join(".omo/mcp.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&serde_json::json!({ "mcpServers": servers })).unwrap(),
    )
    .unwrap();
    path
}

fn reattach(
    service: Arc<tokio::sync::Mutex<McpService>>,
    cwd: &Path,
    agent_dir: &Path,
) -> ManagerReattach {
    let (cwd, agent_dir) = (cwd.to_owned(), agent_dir.to_owned());
    Arc::new(move || {
        let (service, cwd, agent_dir) = (service.clone(), cwd.clone(), agent_dir.clone());
        Box::pin(async move {
            service
                .lock()
                .await
                .attach_session(&cwd, &agent_dir, &Default::default(), true, &[])
                .await
                .map_err(|error| error.to_string())
        })
    })
}

fn no_reattach() -> ManagerReattach {
    Arc::new(|| {
        Box::pin(async { Err::<(), String>("this manager path must not write config".into()) })
    })
}

type Calls = Arc<Mutex<Vec<(McpManagerCommand, String)>>>;

fn recording_run(notice: &'static str) -> (ManagerRun, Calls) {
    let calls: Calls = Arc::default();
    let sink = calls.clone();
    let run: ManagerRun = Arc::new(move |command, name, notify: Option<McpNotify>| {
        sink.lock().unwrap().push((command, name));
        if let Some(notify) = notify {
            notify(notice, maho_ext_api::NotificationType::Info);
        }
        Box::pin(async {})
    });
    (run, calls)
}

// 2716-mcp-manager-write-gate: read-only provenance offers Details and Logs but no writes, and an
// untrusted project's bytes survive Details, Logs, and a clean exit.
#[tokio::test]
async fn untrusted_project_servers_offer_inspection_only_and_keep_their_bytes() {
    let cwd = tempfile::tempdir().unwrap();
    let agent = tempfile::tempdir().unwrap();
    let path = write_project(
        cwd.path(),
        serde_json::json!({"fx": {"command": "/missing/fx", "enabled": true, "exposure": "search",
            "env": {"PLACEHOLDER": "${UNSET:-preserve-me}"}}}),
    );
    let before = std::fs::read(&path).unwrap();
    let service = attach(cwd.path(), agent.path(), false, serde_json::json!({})).await;
    let surface = ScriptedSurface::new(&[
        Some("fx"),
        Some("status"),
        None,
        Some("logs"),
        None,
        None,
        None,
    ]);
    let (run, calls) = recording_run("unused");
    let outcome = manage(&surface, &service, no_reattach(), run).await;
    assert_eq!(outcome, Ok(None));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let menus = surface.menus();
    assert_eq!(menus.len(), 7);
    assert_eq!(surface.values(0), vec!["fx"]);
    assert!(menus[0].items[0]
        .description
        .as_deref()
        .unwrap()
        .contains("project"));
    assert_eq!(surface.values(1), vec!["status", "logs"]);
    assert_eq!(menus[2].title, "Details of fx");
    assert!(!menus[2].items.is_empty());
    assert_eq!(menus[4].title, "Logs of fx");
    assert!(!menus[4].items.is_empty());
    assert!(calls.lock().unwrap().is_empty());
}

// 2716-mcp-manager-write-gate "retains global write actions when enabled/disabled".
#[tokio::test]
async fn global_servers_retain_their_write_actions() {
    for (enabled, toggle) in [(true, "disable"), (false, "enable")] {
        let root = tempfile::tempdir().unwrap();
        let service = attach(
            root.path(),
            root.path(),
            true,
            serde_json::json!({"fx": {"command": "/missing/fx", "enabled": enabled}}),
        )
        .await;
        let surface = ScriptedSurface::new(&[Some("fx"), None, None]);
        let (run, _) = recording_run("unused");
        assert_eq!(manage(&surface, &service, no_reattach(), run).await, Ok(None));
        assert_eq!(surface.menus().len(), 3);
        let actions = surface.values(1);
        assert!(actions.contains(&toggle.to_owned()), "{actions:?}");
        assert!(actions.contains(&"exposure".to_owned()));
        service.lock().await.dispose().await.unwrap();
    }
}

// 2716-mcp-manager "edits only the selected project's config": the project definition shadows the
// global one, its exposure changes, and its interpolation expression survives.
#[tokio::test]
async fn exposure_edits_only_the_selected_project_config_and_keeps_placeholders() {
    let cwd = tempfile::tempdir().unwrap();
    let agent = tempfile::tempdir().unwrap();
    let placeholder = "${UNSET:-fallback}";
    let project = write_project(
        cwd.path(),
        serde_json::json!({"fx": {"command": "/missing/fx", "enabled": false, "exposure": "search",
            "env": {"PLACEHOLDER": placeholder}}}),
    );
    let service = attach(
        cwd.path(),
        agent.path(),
        true,
        serde_json::json!({"fx": {"command": "/missing/fx", "enabled": false, "exposure": "direct"}}),
    )
    .await;
    let surface = ScriptedSurface::new(&[
        Some("fx"),
        Some("exposure"),
        Some("proxy"),
        None,
        None,
    ]);
    let (run, _) = recording_run("unused");
    let outcome = manage(
        &surface,
        &service,
        reattach(service.clone(), cwd.path(), agent.path()),
        run,
    )
    .await;
    assert_eq!(outcome, Ok(None));
    let menus = surface.menus();
    assert!(surface.values(1).contains(&"enable".to_owned()));
    assert_eq!(menus[2].title, "Exposure of fx");
    assert_eq!(menus[2].selected.as_deref(), Some("search"));
    assert_eq!(menus[3].details.as_deref(), Some("Configuration saved."));
    let project: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(project).unwrap()).unwrap();
    let global: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(agent.path().join("mcp.json")).unwrap())
            .unwrap();
    assert_eq!(project["mcpServers"]["fx"]["exposure"], "proxy");
    assert_eq!(project["mcpServers"]["fx"]["env"]["PLACEHOLDER"], placeholder);
    assert_eq!(global["mcpServers"]["fx"]["exposure"], "direct");
    service.lock().await.dispose().await.unwrap();
}

// 2716-mcp-manager-dispatch "preserves raw server identity when the manager selects test/reconnect":
// the existing handler receives the original name and its notices become the menu details.
#[tokio::test]
async fn test_and_reconnect_dispatch_the_raw_server_identity() {
    for name in [r"path\tools\n", "control\t\r\u{8}"] {
        for (action, command) in [
            ("test", McpManagerCommand::Test),
            ("reconnect", McpManagerCommand::Reconnect),
        ] {
            let root = tempfile::tempdir().unwrap();
            let service = attach(
                root.path(),
                root.path(),
                true,
                serde_json::json!({ name: {"command": "/missing/fx", "lifecycle": "lazy"} }),
            )
            .await;
            let surface = ScriptedSurface::new(&[Some(name), Some(action), None, None]);
            let (run, calls) = recording_run("handler notice");
            assert_eq!(manage(&surface, &service, no_reattach(), run).await, Ok(None));
            assert_eq!(*calls.lock().unwrap(), vec![(command, name.to_owned())]);
            let menus = surface.menus();
            assert!(
                menus[2]
                    .details
                    .as_deref()
                    .is_some_and(|details| details.contains("handler notice"))
            );
            service.lock().await.dispose().await.unwrap();
        }
    }
}

// 2716-mcp-manager-auth: the credential-appropriate OAuth command leaves the manager, independent of
// transport health, and nothing runs inside the view.
#[tokio::test]
async fn oauth_actions_follow_stored_credentials_and_leave_the_view() {
    let url = "http://127.0.0.1:1/mcp";
    for (token, expected) in [
        (None, McpManagerCommand::Auth),
        (Some("fake-access"), McpManagerCommand::Logout),
    ] {
        let root = tempfile::tempdir().unwrap();
        if let Some(token) = token {
            maho_ext_mcp::auth::token_store::McpTokenStore::new(root.path(), "fx", url)
                .write(maho_ext_mcp::auth::token_store::McpStoredAuth {
                    access_token: Some(token.into()),
                    ..Default::default()
                })
                .unwrap();
        }
        let service = attach(
            root.path(),
            root.path(),
            true,
            serde_json::json!({"fx": {"type": "http", "url": url, "lifecycle": "lazy"}}),
        )
        .await;
        let action = if expected == McpManagerCommand::Auth {
            "auth"
        } else {
            "logout"
        };
        let surface = ScriptedSurface::new(&[Some("fx"), Some(action)]);
        let (run, calls) = recording_run("unused");
        let outcome = manage(&surface, &service, no_reattach(), run).await;
        assert_eq!(
            outcome,
            Ok(Some(McpManagerAuthAction {
                command: expected,
                name: "fx".into()
            }))
        );
        let offered = surface
            .values(1)
            .into_iter()
            .filter(|value| value == "auth" || value == "logout")
            .collect::<Vec<_>>();
        assert_eq!(offered, vec![action.to_owned()]);
        assert!(calls.lock().unwrap().is_empty());
        service.lock().await.dispose().await.unwrap();
    }
}
