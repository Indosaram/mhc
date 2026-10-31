//! Port of senpi `builtin/mcp/manager.ts` (`showMcpManager`): the interactive `/mcp` server manager.
use std::sync::{Arc, Mutex as StdMutex};

use maho_ext_api::{
    CustomUiFactoryOptions, ExtensionContext, ExtensionFailure, JsonValue, NotificationType,
};
use maho_tui::{components::select_list::SelectItem, tui::Component, utils::wrap_text_with_ansi};
use tokio::sync::{Mutex, broadcast};

use crate::{
    commands::{McpCommandHost, McpManagerCommand, McpNotify, ReattachContext},
    config_schema::{Exposure, McpServerConfig, McpServerSource},
    manager_view::{
        BoxFuture, McpManagerMenu, McpManagerSurface, McpManagerView, MenuBuild, MenuSubscribe,
        mcp_manager_channel,
    },
    service::McpService,
    service_types::{McpServerSnapshot, McpSnapshotConfigState, McpWireAuthStatus},
};

const EXPOSURES: [Exposure; 4] = [
    Exposure::Auto,
    Exposure::Direct,
    Exposure::Search,
    Exposure::Proxy,
];

/// The OAuth command the manager hands back to the transcript flow (upstream `{ command, name }`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct McpManagerAuthAction {
    pub command: McpManagerCommand,
    pub name: String,
}

/// Upstream `run(command, name, ctx, notify)`.
pub type ManagerRun =
    Arc<dyn Fn(McpManagerCommand, String, Option<McpNotify>) -> BoxFuture<'static, ()> + Send + Sync>;
/// Upstream `service.attachSession({ type: "session_start", reason: "reload" }, ctx, pi)`.
pub type ManagerReattach = Arc<dyn Fn() -> BoxFuture<'static, Result<(), String>> + Send + Sync>;

/// Upstream `showMcpManager`.
pub async fn show_mcp_manager(
    ctx: &ExtensionContext,
    host: &McpCommandHost,
) -> Result<(), ExtensionFailure> {
    let render_ui = ctx.ui.clone();
    let (handle, parts) = mcp_manager_channel(Arc::new(move || {
        if render_ui.request_render().is_err() {
            // No repaint path: the view drains its queue on the host's next frame instead.
        }
    }));
    let parts = Arc::new(StdMutex::new(Some(parts)));
    let view_ui = ctx.ui.clone();
    let custom = ctx.ui.custom_factory(
        Arc::new(move |tui_host, theme, keybindings, done| {
            let parts = parts
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            let dimensions = tui_host.dimensions_reader();
            let ui = view_ui.clone();
            let render_request: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(move || {
                if ui.request_render().is_err() {
                    // The custom UI is closing; there is nothing left to repaint.
                }
            });
            let theme = theme.clone();
            let keybindings = keybindings.clone();
            Box::pin(async move {
                let parts = parts
                    .ok_or_else(|| ExtensionFailure::new("The MCP manager view is already open"))?;
                Ok(Box::new(McpManagerView::new(
                    parts,
                    theme,
                    keybindings,
                    dimensions,
                    render_request,
                    done,
                )) as Box<dyn Component>)
            })
        }),
        CustomUiFactoryOptions::default(),
    );
    let run: ManagerRun = {
        let ctx = ctx.clone();
        let host = host.clone();
        Arc::new(move |command, name, notify| {
            let ctx = ctx.clone();
            let host = host.clone();
            Box::pin(async move {
                crate::commands::run_manager_command(command, &name, &ctx, &host, notify).await;
            })
        })
    };
    let reattach: ManagerReattach = {
        let service = host.service.clone();
        let context = ReattachContext::from(ctx);
        Arc::new(move || {
            let service = service.clone();
            let context = context.clone();
            Box::pin(async move {
                let mut service = service.lock().await;
                context
                    .reattach(&mut service)
                    .await
                    .map_err(|error| error.to_string())
            })
        })
    };
    let flow = manage(&handle, &host.service, reattach, run);
    tokio::pin!(flow);
    tokio::pin!(custom);
    let outcome = tokio::select! {
        outcome = &mut flow => {
            handle.finish(JsonValue::Null);
            (&mut custom).await?;
            outcome
        }
        // The view closed before the flow finished; the flow is dropped with it.
        closed = &mut custom => {
            closed?;
            return Ok(());
        }
    };
    match outcome {
        // The existing auth flow retains notices in the transcript; do not hide it behind a custom view.
        Ok(Some(action)) => {
            crate::commands::run_manager_command(action.command, &action.name, ctx, host, None)
                .await;
        }
        Ok(None) => {}
        Err(error) => ctx.ui.notify(&error, NotificationType::Error),
    }
    Ok(())
}

fn wire_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Resolves on any connection state change or completed registration (upstream `subscribe`).
fn subscription(service: Arc<Mutex<McpService>>) -> MenuSubscribe {
    Arc::new(move || {
        let service = service.clone();
        Box::pin(async move {
            let (mut registration, states) = {
                let service = service.lock().await;
                (
                    service.on_registration_changed(),
                    service.connection_state_changes().await,
                )
            };
            let registered = async move {
                if registration.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            };
            tokio::select! {
                () = registered => {}
                () = any_state_change(states) => {}
            }
        })
    })
}

async fn any_state_change<T: Clone + Send + 'static>(receivers: Vec<broadcast::Receiver<T>>) {
    if receivers.is_empty() {
        return std::future::pending().await;
    }
    let waits = receivers.into_iter().map(|mut receiver| {
        Box::pin(async move {
            if let Err(broadcast::error::RecvError::Closed) = receiver.recv().await {
                std::future::pending::<()>().await;
            }
        })
    });
    futures::future::select_all(waits).await;
}

async fn server_list(service: &Mutex<McpService>) -> McpManagerMenu {
    let service = service.lock().await;
    let mut items = Vec::new();
    for snapshot in service.server_snapshots().await {
        let exposure = service.exposure_status(&snapshot.name).await;
        let resources = service.registration().and_then(|registration| {
            registration
                .resource_servers
                .iter()
                .find(|server| server.server == snapshot.name)
                .map(|server| server.resources.len())
        });
        let mut description = vec![
            if snapshot.config_state == McpSnapshotConfigState::Enabled {
                wire_name(&snapshot.lifecycle_state)
            } else {
                wire_name(&snapshot.config_state)
            },
            format!(
                "{} tools",
                exposure
                    .tool_count
                    .map_or_else(|| "?".to_owned(), |count| count.to_string())
            ),
        ];
        if let Some(resources) = resources {
            description.push(format!("{resources} resources"));
        }
        description.push(wire_name(
            &exposure
                .mode
                .or_else(|| {
                    service
                        .auth_target(&snapshot.name)
                        .and_then(|target| target.config.exposure)
                })
                .unwrap_or_default(),
        ));
        description.push(
            snapshot
                .source
                .map_or_else(|| "unknown".to_owned(), |source| source.to_string()),
        );
        items.push(SelectItem {
            value: snapshot.name.clone(),
            label: snapshot.name.clone(),
            description: Some(description.join(" · ")),
        });
    }
    McpManagerMenu {
        title: "MCP servers".into(),
        items,
        empty: Some(
            "No MCP servers configured. Add one with /mcp add <name> <command...|url>.".into(),
        ),
        confirm_label: "manage".into(),
        cancel_label: "close".into(),
        ..Default::default()
    }
}

async fn find_snapshot(service: &Mutex<McpService>, name: &str) -> Option<McpServerSnapshot> {
    service
        .lock()
        .await
        .server_snapshots()
        .await
        .into_iter()
        .find(|snapshot| snapshot.name == name)
}

/// Upstream `manage`: the server list, then each server's action menu, until the list is closed.
pub async fn manage(
    view: &dyn McpManagerSurface,
    service: &Arc<Mutex<McpService>>,
    reattach: ManagerReattach,
    run: ManagerRun,
) -> Result<Option<McpManagerAuthAction>, String> {
    let subscribe = subscription(service.clone());
    loop {
        let list_service = service.clone();
        let build: MenuBuild = Arc::new(move || {
            let service = list_service.clone();
            Box::pin(async move { Ok(server_list(&service).await) })
        });
        let Some(name) = view.menu(build, Some(subscribe.clone())).await? else {
            return Ok(None);
        };
        let mut message: Option<String> = None;
        loop {
            let action_service = service.clone();
            let action_name = name.clone();
            let action_message = message.clone();
            let build: MenuBuild = Arc::new(move || {
                let service = action_service.clone();
                let name = action_name.clone();
                let message = action_message.clone();
                Box::pin(async move {
                    let service = service.lock().await;
                    let snapshot = service
                        .server_snapshots()
                        .await
                        .into_iter()
                        .find(|snapshot| snapshot.name == name);
                    let config = service.auth_target(&name).map(|target| target.config);
                    let logged_in = service.server_auth_status(&name).await == McpWireAuthStatus::OAuth;
                    let details = [
                        snapshot.as_ref().and_then(|snapshot| snapshot.last_error.clone()),
                        message,
                    ]
                    .into_iter()
                    .flatten()
                    .filter(|text| !text.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                    Ok(McpManagerMenu {
                        title: format!("MCP server {name}"),
                        details: (!details.is_empty()).then_some(details),
                        items: snapshot.as_ref().map_or_else(Vec::new, |snapshot| {
                            server_actions(snapshot, config.as_ref(), logged_in)
                        }),
                        empty: Some("This server is no longer configured.".into()),
                        confirm_label: "select".into(),
                        cancel_label: "back".into(),
                        ..Default::default()
                    })
                })
            });
            let Some(action) = view.menu(build, Some(subscribe.clone())).await? else {
                break;
            };
            let Some(snapshot) = find_snapshot(service, &name).await else {
                break;
            };
            if action == "auth" || action == "logout" {
                return Ok(Some(McpManagerAuthAction {
                    command: if action == "auth" {
                        McpManagerCommand::Auth
                    } else {
                        McpManagerCommand::Logout
                    },
                    name,
                }));
            }
            message = match perform(view, service, &reattach, &run, &name, &action, &snapshot).await
            {
                Ok(message) => message,
                Err(error) => Some(error),
            };
        }
    }
}

/// One selected action; `Ok(None)` mirrors upstream's `continue` after `message = undefined`.
async fn perform(
    view: &dyn McpManagerSurface,
    service: &Arc<Mutex<McpService>>,
    reattach: &ManagerReattach,
    run: &ManagerRun,
    name: &str,
    action: &str,
    snapshot: &McpServerSnapshot,
) -> Result<Option<String>, String> {
    match action {
        "status" => {
            let exposure = service.lock().await.exposure_status(name).await;
            let rows = [crate::status::McpStatusRow {
                snapshot: snapshot.clone(),
                exposure,
            }];
            let text = crate::status::format_mcp_status("", &rows);
            show_text(view, &format!("Details of {name}"), text.trim()).await?;
            Ok(None)
        }
        "logs" => {
            let lines = service.lock().await.log_lines(name, 20).await;
            let text = if lines.is_empty() {
                "(empty)".to_owned()
            } else {
                lines.join("\n")
            };
            show_text(view, &format!("Logs of {name}"), &text).await?;
            Ok(None)
        }
        "tools" => {
            let Some(client) = service.lock().await.connected_client(name).await else {
                return Ok(None);
            };
            let tools = crate::catalog::collect_client_pages(
                &client,
                "tools/list",
                "tools",
                std::time::Duration::from_secs(2),
            )
            .await
            .map_err(|error| error.to_string())?;
            let items = tools
                .items
                .iter()
                .map(|tool| {
                    let name = tool
                        .get("name")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    SelectItem {
                        value: name.clone(),
                        label: name,
                        description: tool
                            .get("description")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_owned),
                    }
                })
                .collect::<Vec<_>>();
            let title = format!("Tools of {name}");
            view.menu(
                Arc::new(move || {
                    let menu = McpManagerMenu {
                        title: title.clone(),
                        items: items.clone(),
                        empty: Some("The server offers no tools.".into()),
                        confirm_label: "back".into(),
                        cancel_label: "back".into(),
                        ..Default::default()
                    };
                    Box::pin(async move { Ok(menu) })
                }),
                None,
            )
            .await?;
            Ok(None)
        }
        "enable" | "disable" | "exposure" => {
            let mut exposure = None;
            if action == "exposure" {
                let current = service
                    .lock()
                    .await
                    .auth_target(name)
                    .and_then(|target| target.config.exposure);
                let title = format!("Exposure of {name}");
                let choice = view
                    .menu(
                        Arc::new(move || {
                            let menu = McpManagerMenu {
                                title: title.clone(),
                                items: EXPOSURES
                                    .iter()
                                    .map(|value| SelectItem {
                                        value: wire_name(value),
                                        label: wire_name(value),
                                        description: None,
                                    })
                                    .collect(),
                                selected: current.as_ref().map(wire_name),
                                confirm_label: "save".into(),
                                cancel_label: "back".into(),
                                ..Default::default()
                            };
                            Box::pin(async move { Ok(menu) })
                        }),
                        None,
                    )
                    .await?;
                exposure = EXPOSURES
                    .into_iter()
                    .find(|value| choice.as_deref() == Some(wire_name(value).as_str()));
                if exposure.is_none() {
                    return Ok(None);
                }
            }
            let Some(path) = snapshot.source_path.as_ref().filter(|_| is_writable(snapshot))
            else {
                return Ok(None);
            };
            let enabled = exposure.is_none().then_some(action == "enable");
            if !crate::config_edit::update_mcp_server_config(path, name, enabled, exposure)
                .map_err(|error| error.0)?
            {
                return Ok(Some(
                    "This server is no longer in its configuration file.".into(),
                ));
            }
            view.status(&format!("MCP server {name}"), "Applying configuration...");
            reattach().await?;
            Ok(Some("Configuration saved.".into()))
        }
        "test" | "reconnect" => {
            view.status(
                &format!("MCP server {name}"),
                if action == "test" {
                    "Testing..."
                } else {
                    "Reconnecting..."
                },
            );
            let notices = Arc::new(StdMutex::new(Vec::<String>::new()));
            let sink = notices.clone();
            let notify: McpNotify = Arc::new(move |text, _| {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(text.to_owned());
            });
            let command = if action == "test" {
                McpManagerCommand::Test
            } else {
                McpManagerCommand::Reconnect
            };
            run(command, name.to_owned(), Some(notify)).await;
            let text = notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .join("\n");
            Ok(Some(text))
        }
        _ => Ok(None),
    }
}

fn is_writable(snapshot: &McpServerSnapshot) -> bool {
    snapshot.config_state != McpSnapshotConfigState::Untrusted
        && matches!(
            snapshot.source,
            Some(McpServerSource::Global | McpServerSource::Project)
        )
}

/// Upstream `serverActions`.
pub fn server_actions(
    snapshot: &McpServerSnapshot,
    config: Option<&McpServerConfig>,
    logged_in: bool,
) -> Vec<SelectItem> {
    let item = |value: &str, label: &str, description: Option<String>| SelectItem {
        value: value.into(),
        label: label.into(),
        description,
    };
    let mut items = vec![item("status", "Details", None), item("logs", "Logs", None)];
    if snapshot.config_state == McpSnapshotConfigState::Enabled {
        if snapshot.lifecycle_state
            == crate::service_types::McpLifecycleState::Connection(
                crate::connection::ServerConnectionState::Connected,
            )
        {
            items.push(item("tools", "Tools", None));
        }
        items.push(item("test", "Test connection", None));
        items.push(item("reconnect", "Reconnect", None));
        if config.is_some_and(|config| {
            crate::auth::context::resolve_auth_mode(config)
                == crate::auth::context::ServerAuthMode::OAuth
        }) {
            items.push(if logged_in {
                item(
                    "logout",
                    "Sign out",
                    Some("deletes stored credentials".into()),
                )
            } else {
                item(
                    "auth",
                    "Sign in",
                    Some("closes this view and opens authorization".into()),
                )
            });
        }
    }
    if is_writable(snapshot) {
        let disabled = snapshot.config_state == McpSnapshotConfigState::Disabled;
        let source = snapshot
            .source
            .map_or_else(String::new, |source| source.to_string());
        items.push(item(
            if disabled { "enable" } else { "disable" },
            if disabled { "Enable" } else { "Disable" },
            Some(format!("saved to {source} mcp.json")),
        ));
        items.push(item(
            "exposure",
            "Exposure",
            config.and_then(|config| config.exposure.as_ref().map(wire_name)),
        ));
    }
    items
}

/// Upstream `showText`: a read-only menu of the wrapped text lines.
async fn show_text(view: &dyn McpManagerSurface, title: &str, text: &str) -> Result<(), String> {
    let items = wrap_text_with_ansi(text, view.columns().saturating_sub(4).max(1))
        .into_iter()
        .enumerate()
        .map(|(index, label)| SelectItem {
            value: index.to_string(),
            label,
            description: None,
        })
        .collect::<Vec<_>>();
    let title = title.to_owned();
    view.menu(
        Arc::new(move || {
            let menu = McpManagerMenu {
                title: title.clone(),
                items: items.clone(),
                confirm_label: "back".into(),
                cancel_label: "back".into(),
                ..Default::default()
            };
            Box::pin(async move { Ok(menu) })
        }),
        None,
    )
    .await?;
    Ok(())
}
