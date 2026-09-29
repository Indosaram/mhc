use super::*;
use crate::lsp::errors::LspError;
use crate::lsp::types::FailedServerLookupResult;
use crate::lsp::types::ServerLookupInfo;
use crate::request_context::join;
use crate::request_context::parse_lsp_request_context;
use crate::request_context::run_with_request_context;
use pretty_assertions::assert_eq;

fn home() -> String {
    std::env::var("HOME").expect("HOME")
}

fn details(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected object, got {other}"),
    }
}

#[test]
fn not_configured_lookup_includes_structured_availability_and_typed_config_paths() {
    let dir = tempfile::Builder::new()
        .prefix("lsp-missing-dep-")
        .tempdir_in(home())
        .expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();
    let project = join(&root, &[".codex", "lsp-client.json"]);
    let user = join(&home(), &[".codex", "lsp-client.json"]);
    let decisions = join(&home(), &[".codex", "lsp-install-decisions.json"]);
    let context = parse_lsp_request_context(&json!({
        "cwd": root, "projectConfigPaths": [project], "userConfigPath": user,
        "installDecisionsPath": decisions, "capabilities": { "installDecisionTool": false },
    }))
    .expect("context");
    let error = LspError::ServerLookup {
        message: "No LSP server configured for extension: .foo".to_string(),
        lookup: Some(FailedServerLookupResult::NotConfigured {
            extension: ".foo".to_string(),
            available_servers: vec!["typescript".to_string()],
        }),
    };

    let result = run_with_request_context(context, || {
        missing_dependency_result(&error, details(json!({ "filePath": "src/file.foo" })))
    })
    .expect("missing dependency");

    assert_eq!(
        Value::Object(result.details),
        json!({
            "filePath": "src/file.foo",
            "error": "No LSP server configured for extension: .foo",
            "errorKind": "missing_dependency",
            "availability": {
                "kind": "not_configured",
                "extension": ".foo",
                "availableServers": ["typescript"],
                "projectConfigPaths": [project],
                "userConfigPath": user,
                "installDecisionTool": false,
            },
        })
    );
}

#[test]
fn not_installed_lookup_includes_install_decision_availability() {
    let dir = tempfile::Builder::new()
        .prefix("lsp-missing-dep-")
        .tempdir_in(home())
        .expect("tempdir");
    let root = dir.path().to_string_lossy().into_owned();
    let decisions = join(&home(), &[".codex", "lsp-install-decisions.json"]);
    let context = parse_lsp_request_context(&json!({
        "cwd": root,
        "projectConfigPaths": [join(&root, &[".codex", "lsp-client.json"])],
        "userConfigPath": join(&home(), &[".codex", "lsp-client.json"]),
        "installDecisionsPath": decisions,
        "capabilities": { "installDecisionTool": true },
    }))
    .expect("context");
    let error = LspError::ServerLookup {
        message: "LSP server 'typescript' for .ts is NOT INSTALLED.".to_string(),
        lookup: Some(FailedServerLookupResult::NotInstalled {
            server: ServerLookupInfo {
                id: "typescript".to_string(),
                command: vec![
                    "typescript-language-server".to_string(),
                    "--stdio".to_string(),
                ],
                extensions: vec![".ts".to_string()],
            },
            install_hint: "npm install -g typescript-language-server typescript".to_string(),
        }),
    };

    let result = run_with_request_context(context, || {
        missing_dependency_result(&error, details(json!({ "filePath": "src/file.ts" })))
    })
    .expect("missing dependency");

    assert_eq!(
        Value::Object(result.details),
        json!({
            "filePath": "src/file.ts",
            "error": "LSP server 'typescript' for .ts is NOT INSTALLED.",
            "errorKind": "missing_dependency",
            "availability": {
                "kind": "not_installed",
                "serverId": "typescript",
                "command": ["typescript-language-server", "--stdio"],
                "extensions": [".ts"],
                "installHint": "npm install -g typescript-language-server typescript",
                "installDecisionTool": true,
                "installDecisionsPath": decisions,
            },
        })
    );
}
