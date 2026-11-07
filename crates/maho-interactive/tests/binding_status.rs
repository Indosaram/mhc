//! senpi v2026.10.27-2 `bindCurrentSessionExtensions` / `subscribeToAgent` (interactive-mode.ts):
//! what a binding shows in the footer and which presentation it registers.
//!
//! - the bound session's `session_lifecycle` records drive the footer; records naming another session
//!   are dropped (fae4cb50ec, 3ffffa7369);
//! - a session already paused when it is bound shows its pause footer before any pause event
//!   (14bf31d76c);
//! - each binding registers the terminal UI as its MCP MRTR presentation, under a lifetime that ends
//!   with the terminal (senpi `mcpMrtrPresentationLifetime`).

use maho_interactive::lifecycle_status::LIFECYCLE_STATUS_KEY;
use maho_interactive::pause_footer::PAUSE_STATUS_KEY;
use serde_json::json;
use std::sync::Arc;

fn status(mode: &maho_interactive::interactive_mode::InteractiveMode, key: &str) -> Option<String> {
    mode.extension_ui
        .statuses
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(key)
        .cloned()
}

async fn bound_mode() -> (
    maho_interactive::interactive_mode::InteractiveMode,
    tempfile::TempDir,
    Arc<maho_core::agent_session::AgentSession>,
) {
    let (mut mode, directory, session) =
        crate::runtime35_native::native_mode_with_session_and_provider_retaining_session(
            None,
            |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
            "faux",
            "faux-1",
            false,
        );
    tokio::time::timeout(std::time::Duration::from_secs(10), mode.bind_extensions())
        .await
        .expect("bounded binding");
    (mode, directory, session)
}

#[tokio::test]
async fn the_bound_sessions_lifecycle_records_drive_the_footer_and_foreign_ones_are_dropped() {
    let (mut mode, _directory, session) = bound_mode().await;
    let bound = status(&mode, LIFECYCLE_STATUS_KEY);
    let seq = session.lifecycle_snapshot()["seq"].as_u64().unwrap_or(0);
    // A record for another session on the same bus changes nothing.
    session.emit_extension_event(
        "session_lifecycle",
        &json!({"type": "session_lifecycle", "sessionId": "someone-else",
            "lifecycle": {"state": "failed", "seq": seq + 10}}),
    );
    mode.drain_events();
    assert_eq!(status(&mode, LIFECYCLE_STATUS_KEY), bound);
    // A newer record of the bound session replaces the footer.
    session.emit_extension_event(
        "session_lifecycle",
        &json!({"type": "session_lifecycle", "sessionId": session.session_id(),
            "lifecycle": {"state": "waiting_children", "seq": seq + 1,
                "waitingOn": {"children": 2}}}),
    );
    mode.drain_events();
    assert_eq!(
        status(&mode, LIFECYCLE_STATUS_KEY).as_deref(),
        Some("waiting_children · children: 2")
    );
}

#[tokio::test]
async fn a_session_paused_before_binding_shows_its_pause_footer_at_bind() {
    let (mut mode, _directory, session) =
        crate::runtime35_native::native_mode_with_session_and_provider_retaining_session(
            None,
            |cwd| maho_core::session_manager::SessionManager::in_memory(cwd, None, None),
            "faux",
            "faux-1",
            false,
        );
    session
        .fence_run(json!({"kind": "task", "id": "t1"}))
        .expect("run fence");
    assert_eq!(status(&mode, PAUSE_STATUS_KEY), None);
    tokio::time::timeout(std::time::Duration::from_secs(10), mode.bind_extensions())
        .await
        .expect("bounded binding");
    assert_eq!(
        status(&mode, PAUSE_STATUS_KEY).as_deref(),
        Some("Run paused · 0 updates held · type to continue, /resume to resume the run")
    );
}

#[tokio::test]
async fn disposed_lifecycle_binding_discards_queued_and_late_records() {
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = bound_mode().await;
    assert!(status(&mode, LIFECYCLE_STATUS_KEY).is_some());
    let seq = session.lifecycle_snapshot()["seq"].as_u64().unwrap_or(0);
    let record = json!({"type":"session_lifecycle", "sessionId":session.session_id(),
        "lifecycle":{"state":"waiting_children", "seq":seq + 10,
            "waitingOn":{"children":2}}});
    session.emit_extension_event("session_lifecycle", &record);
    mode.dispose();
    assert_eq!(status(&mode, LIFECYCLE_STATUS_KEY), None);
    session.emit_extension_event("session_lifecycle", &record);
    mode.drain_events();
    assert_eq!(status(&mode, LIFECYCLE_STATUS_KEY), None);
}

#[tokio::test]
async fn reload_preserves_host_lifecycle_status_and_receives_later_records() {
    use maho_ext_api::ExtensionUi;
    use maho_tui::tui::Component;
    let (mut mode, _directory, session) = bound_mode().await;
    let initial = status(&mode, LIFECYCLE_STATUS_KEY);
    assert!(initial.is_some());
    mode.extension_ui
        .set_status("extension-status", Some("temporary"));
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        assert_eq!(
            mode.submit("/reload", Default::default())
                .await
                .expect("reload command"),
            maho_core::agent_session::PromptDisposition::Handled
        );
        mode.render(100);
        assert_eq!(
            mode.submit_editor().await.expect("queued reload"),
            Some(maho_core::agent_session::PromptDisposition::Handled)
        );
    })
    .await
    .expect("reload deadline");
    assert_eq!(status(&mode, LIFECYCLE_STATUS_KEY), initial);
    assert_eq!(status(&mode, "extension-status"), None);
    let seq = session.lifecycle_snapshot()["seq"].as_u64().unwrap_or(0);
    session.emit_extension_event(
        "session_lifecycle",
        &json!({
            "type":"session_lifecycle", "sessionId":session.session_id(),
            "lifecycle":{"state":"waiting_background", "seq":seq + 1}
        }),
    );
    mode.drain_events();
    assert_eq!(
        status(&mode, LIFECYCLE_STATUS_KEY).as_deref(),
        Some("waiting_background")
    );
    mode.dispose();
}

#[tokio::test]
async fn each_binding_registers_the_terminal_presentation_until_the_terminal_ends() {
    use maho_tui::tui::Component;
    let (mut mode, _directory, _session) = bound_mode().await;
    let ui: Arc<dyn maho_ext_api::ExtensionUi> = mode.extension_ui.clone();
    assert!(maho_core::mcp_mrtr_presentations::get_mcp_mrtr_presentation(&ui).is_some());
    // A rebind replaces the lifetime: the presentation stays registered for the new binding.
    tokio::time::timeout(std::time::Duration::from_secs(10), mode.bind_extensions())
        .await
        .expect("bounded rebind");
    assert!(maho_core::mcp_mrtr_presentations::get_mcp_mrtr_presentation(&ui).is_some());
    mode.dispose();
    assert!(maho_core::mcp_mrtr_presentations::get_mcp_mrtr_presentation(&ui).is_none());
}
