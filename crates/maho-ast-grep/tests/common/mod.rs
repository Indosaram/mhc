//! Shared helpers: a real or fake `ast-grep`, a server over it, and tree builders.
#![allow(dead_code)]

use maho_ast_grep::resolve::Toolchain;
use maho_ast_grep::server::ServerOptions;
use maho_ast_grep::{Context, Server, abort_channel, handle_message};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// A server whose program lookup is exactly `toolchain`.
pub fn server_with(toolchain: Toolchain, project_cwd: Option<PathBuf>) -> Server {
    Server::new(ServerOptions {
        context: Context {
            toolchain,
            project_cwd,
        },
        idle_timeout: None,
    })
}

/// A server that uses the real `ast-grep` found the usual way. Tests built on it fail, not skip,
/// when the program is missing.
pub fn real_server(cwd: &Path) -> Server {
    server_with(Toolchain::from_env(), Some(cwd.to_path_buf()))
}

/// A toolchain that only knows `program` (as the override).
pub fn only(program: &Path) -> Toolchain {
    Toolchain {
        override_path: Some(program.to_string_lossy().into_owned()),
        platform: "linux".into(),
        arch: "x64".into(),
        ..Toolchain::default()
    }
}

/// Call a tool through the JSON-RPC layer and return the parsed result object plus `isError`.
pub async fn call(server: &Server, tool: &str, arguments: Value) -> (Value, bool) {
    let request = json!({
        "jsonrpc": "2.0", "id": 1, "method": "tools/call",
        "params": { "name": tool, "arguments": arguments }
    });
    let (_handle, mut signal) = abort_channel();
    let reply = handle_message(server, &request.to_string(), &mut signal)
        .await
        .expect("a reply");
    let result = &reply["result"];
    let text = result["content"][0]["text"].as_str().expect("text content");
    let value: Value = serde_json::from_str(text).expect("result text is JSON");
    (value, result["isError"].as_bool().expect("isError"))
}

pub fn code_of(value: &Value) -> &str {
    value["error"]["code"].as_str().unwrap_or_default()
}

/// Write `files` (relative path, content) under `dir`.
pub fn tree(dir: &Path, files: &[(&str, &str)]) {
    for (name, content) in files {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, content).expect("write file");
    }
}

/// One JSON search record as `ast-grep --json=stream` prints it.
pub fn record(file: &str, start: u64) -> String {
    json!({
        "text": "x()",
        "range": {
            "byteOffset": { "start": start, "end": start + 3 },
            "start": { "line": 0, "column": start },
            "end": { "line": 0, "column": start + 3 }
        },
        "file": file,
        "language": "JavaScript",
        "metaVariables": { "single": {}, "multi": {}, "transformed": {} }
    })
    .to_string()
}

#[cfg(unix)]
pub mod fake {
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    /// A shell script named `name` that answers `--version` like ast-grep and otherwise runs
    /// `body`.
    pub fn program(dir: &Path, name: &str, body: &str) -> PathBuf {
        script(dir, name, "ast-grep 0.0.0-fake", body)
    }

    pub fn script(dir: &Path, name: &str, version_text: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        let text = format!(
            "#!/bin/sh\ncase \"$1\" in --version) echo '{version_text}'; exit 0;; esac\n{body}\n"
        );
        std::fs::write(&path, text).expect("write fake program");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }
}
