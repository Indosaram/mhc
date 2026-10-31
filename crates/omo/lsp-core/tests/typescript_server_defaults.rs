//! Translation of `typescript-server-defaults.test.ts`.

use lsp_core::lsp::config_loader::ServerSource;
use lsp_core::lsp::config_loader::ServerWithSource;
use lsp_core::lsp::config_loader::get_merged_servers;
use lsp_core::lsp::server_definitions::builtin_server;
use lsp_core::request_context::LspRequestCapabilities;
use lsp_core::request_context::LspRequestContext;
use lsp_core::request_context::run_with_request_context;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;

fn lean_typescript_initialization() -> Value {
    json!({
        "tsserver": { "useSyntaxServer": "never" },
        "disableAutomaticTypingAcquisition": true,
    })
}

fn merged_typescript(project: Option<Value>, user: Option<Value>) -> ServerWithSource {
    let directory = tempfile::tempdir().expect("temp dir");
    let project_path = directory.path().join("project-lsp.json");
    let user_path = directory.path().join("user-lsp.json");
    if let Some(project) = project {
        std::fs::write(&project_path, project.to_string()).expect("write project config");
    }
    if let Some(user) = user {
        std::fs::write(&user_path, user.to_string()).expect("write user config");
    }
    let context = LspRequestContext {
        cwd: directory.path().to_string_lossy().into_owned(),
        project_config_paths: vec![project_path.to_string_lossy().into_owned()],
        user_config_path: user_path.to_string_lossy().into_owned(),
        install_decisions_path: directory
            .path()
            .join("install-decisions.json")
            .to_string_lossy()
            .into_owned(),
        capabilities: LspRequestCapabilities {
            install_decision_tool: false,
        },
    };
    let servers = run_with_request_context(context, get_merged_servers).expect("request context");
    servers
        .into_iter()
        .find(|server| server.server.id == "typescript")
        .expect("typescript server")
}

fn initialization(server: &ServerWithSource) -> Option<Value> {
    server.server.initialization.clone().map(Value::Object)
}

#[test]
fn given_the_builtin_definition_then_its_default_config_turns_off_the_syntax_server_and_automatic_typings()
 {
    let builtin = builtin_server("typescript").expect("typescript builtin");
    assert_eq!(
        builtin.initialization_map().map(Value::Object),
        Some(lean_typescript_initialization())
    );
}

#[test]
fn given_no_config_when_merging_servers_then_the_builtin_typescript_server_carries_the_defaults() {
    let server = merged_typescript(None, None);
    assert_eq!(server.source, ServerSource::Builtin);
    assert_eq!(
        initialization(&server),
        Some(lean_typescript_initialization())
    );
}

#[test]
fn given_a_user_entry_that_only_changes_priority_then_the_defaults_survive() {
    let server = merged_typescript(
        None,
        Some(json!({ "lsp": { "typescript": { "priority": 5 } } })),
    );
    assert_eq!(server.source, ServerSource::User);
    assert_eq!(
        initialization(&server),
        Some(lean_typescript_initialization())
    );
}

#[test]
fn given_a_project_entry_with_its_own_initialization_then_its_keys_win_over_the_defaults() {
    let server = merged_typescript(
        Some(
            json!({ "lsp": { "typescript": { "initialization": { "tsserver": { "useSyntaxServer": "auto" }, "hostInfo": "x" } } } }),
        ),
        None,
    );
    assert_eq!(server.source, ServerSource::Project);
    assert_eq!(
        initialization(&server),
        Some(json!({
            "tsserver": { "useSyntaxServer": "auto" },
            "disableAutomaticTypingAcquisition": true,
            "hostInfo": "x",
        }))
    );
}

#[test]
fn given_a_user_entry_with_only_a_custom_command_then_no_builtin_defaults_are_injected() {
    let server = merged_typescript(
        None,
        Some(json!({ "lsp": { "typescript": { "command": ["vtsls", "--stdio"] } } })),
    );
    assert_eq!(
        server.server.command,
        vec!["vtsls".to_string(), "--stdio".to_string()]
    );
    assert_eq!(initialization(&server), None);
}

#[test]
fn given_a_user_entry_with_a_custom_command_then_no_builtin_defaults_are_injected() {
    let server = merged_typescript(
        None,
        Some(
            json!({ "lsp": { "typescript": { "command": ["custom-ts-ls", "--stdio"], "extensions": [".ts"] } } }),
        ),
    );
    assert_eq!(
        server.server.command,
        vec!["custom-ts-ls".to_string(), "--stdio".to_string()]
    );
    assert_eq!(initialization(&server), None);
}
