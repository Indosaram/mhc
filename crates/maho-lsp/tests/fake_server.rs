//! End-to-end against `tests/fixtures/fake_lsp.py` (skipped when python3 is unavailable).
use maho_lsp::client::ClientTimings;
use maho_lsp::config::ConfigPaths;
use maho_lsp::hooks::{HookChain, Mutation};
use maho_lsp::post_edit::LspDiagnosticsHook;
use maho_lsp::tools::LspTools;
use maho_lsp::{LspError, LspManager, LspManagerOptions};
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn python() -> Option<PathBuf> {
    std::env::var_os("PATH")
        .into_iter()
        .flat_map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .map(|dir| dir.join("python3"))
        .find(|candidate| candidate.is_file())
}

fn fixture() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/fake_lsp.py")
        .display()
        .to_string()
}

fn workspace(python: &Path) -> (tempfile::TempDir, LspTools) {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(".git"), "").expect("marker");
    let config = dir.path().join("lsp-client.json");
    let entry = serde_json::json!({"lsp": {"fake": {"command": [python.display().to_string(), fixture()], "extensions": [".zig"], "initTimeoutMs": 20000}}});
    std::fs::write(&config, entry.to_string()).expect("config");
    let options = LspManagerOptions {
        timings: ClientTimings::immediate(),
        ..LspManagerOptions::default()
    };
    let tools = LspTools::new(
        LspManager::new(options),
        ConfigPaths {
            project: vec![config],
            user: vec![],
        },
    );
    (dir, tools)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_against_fake_server() {
    let Some(python) = python() else { return };
    let (dir, tools) = workspace(&python);
    let cwd = dir.path();
    std::fs::write(cwd.join("a.zig"), "let foo = 1;\nfoo ERROR\nWARN\n").expect("source");

    let output = tools
        .diagnostics(cwd, "a.zig", None)
        .await
        .expect("diagnostics");
    assert_eq!(
        output.text,
        "error[fake] at 2:4: bad token\nwarning at 3:0: careful"
    );
    assert_eq!(output.details["totalDiagnostics"], 2);
    let errors = tools
        .diagnostics(cwd, "a.zig", Some("error"))
        .await
        .expect("errors");
    assert_eq!(errors.text, "error[fake] at 2:4: bad token");

    let path = cwd.join("a.zig").canonicalize().expect("canonical");
    let definition = tools
        .goto_definition(cwd, "a.zig", 1, 5)
        .await
        .expect("definition");
    assert!(
        definition.text.ends_with("a.zig:1:4"),
        "{}",
        definition.text
    );
    let references = tools
        .find_references(cwd, "a.zig", 1, 5, true)
        .await
        .expect("references");
    assert_eq!(references.details["totalReferences"], 2);
    let symbols = tools
        .symbols(cwd, "a.zig", "document", None, None)
        .await
        .expect("symbols");
    assert_eq!(
        symbols.text,
        "Outer (Class) - line 1\n  inner (Method) - line 2"
    );
    let workspace = tools
        .symbols(cwd, "a.zig", "workspace", Some("needle"), None)
        .await
        .expect("workspace symbols");
    assert_eq!(workspace.text, "needle (Function) - /w/x.fake:3:1");
    let missing = tools
        .symbols(cwd, "a.zig", "workspace", None, None)
        .await
        .expect("missing query");
    assert_eq!(missing.details["errorKind"], "missing_query");
    let prepare = tools
        .prepare_rename(cwd, "a.zig", 1, 5)
        .await
        .expect("prepare");
    assert_eq!(
        prepare.text,
        "Rename available at 1:4-1:7 (current: \"foo\")"
    );
    let rename = tools
        .rename(cwd, "a.zig", 1, 5, "bar")
        .await
        .expect("rename");
    assert!(
        rename.text.starts_with("Applied 1 edit(s) to 1 file(s):"),
        "{}",
        rename.text
    );
    assert!(
        std::fs::read_to_string(&path)
            .expect("renamed")
            .starts_with("let bar = 1;")
    );
    assert_eq!(tools.manager.client_count(), 1);

    std::fs::write(cwd.join("a.zig"), "clean\n").expect("rewrite");
    let clean = tools
        .diagnostics(cwd, &path.display().to_string(), None)
        .await
        .expect("synced");
    assert_eq!(clean.text, "No diagnostics found");

    std::fs::create_dir(cwd.join("sub")).expect("sub");
    std::fs::write(cwd.join("sub/b.zig"), "ERROR\n").expect("b");
    std::fs::write(cwd.join("sub/c.zig"), "ok\n").expect("c");
    let directory = tools
        .diagnostics(cwd, "sub", Some("error"))
        .await
        .expect("directory");
    assert!(
        directory.text.contains("Files scanned: 2"),
        "{}",
        directory.text
    );
    assert!(
        directory.text.contains("Total diagnostics: 1"),
        "{}",
        directory.text
    );
    assert!(
        directory
            .text
            .contains("b.zig: error[fake] at 1:0: bad token"),
        "{}",
        directory.text
    );

    let chain = HookChain::default().with_hook(Arc::new(LspDiagnosticsHook {
        tools: tools.clone(),
    }));
    let mutation = Mutation {
        tool_name: "write".into(),
        tool_call_id: "1".into(),
        cwd: cwd.to_path_buf(),
        files: vec!["sub/b.zig".into(), "sub/c.zig".into(), "notes.md".into()],
    };
    let appended = chain.run(&mutation).await;
    assert_eq!(
        appended,
        vec![
            "\n\nLSP errors detected in sub/b.zig, please fix:\nerror[fake] at 1:0: bad token"
                .to_owned()
        ]
    );
    let ignored = Mutation {
        tool_name: "bash".into(),
        ..mutation
    };
    assert!(chain.run(&ignored).await.is_empty());

    tools.manager.stop_all().await;
    assert_eq!(tools.manager.client_count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dead_server_is_retried_once_then_reported() {
    let Some(python) = python() else { return };
    let (dir, tools) = workspace(&python);
    std::fs::write(dir.path().join("boom.zig"), "CRASH\n").expect("source");
    let error = tools
        .diagnostics(dir.path(), "boom.zig", None)
        .await
        .expect_err("crash");
    assert!(
        matches!(
            error,
            LspError::ProcessExited { .. } | LspError::ConnectionClosed { .. }
        ),
        "{error:?}"
    );
    std::fs::write(dir.path().join("ok.zig"), "fine\n").expect("source");
    let output = tools
        .diagnostics(dir.path(), "ok.zig", None)
        .await
        .expect("fresh server");
    assert_eq!(output.text, "No diagnostics found");
    tools.manager.stop_all().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_reuses_idle_server_and_caches_initialize() {
    use maho_lsp::daemon::{self, CONNECT_METHOD, ConnectParams, DaemonOptions};
    use maho_lsp::rpc::{read_frame, write_frame};
    use serde_json::{Value, json};
    use tokio::io::BufReader;

    let Some(python) = python() else { return };
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let serve_socket = socket.clone();
    let options = DaemonOptions {
        server_idle: std::time::Duration::from_secs(30),
        daemon_idle: std::time::Duration::from_secs(30),
        reap_interval: std::time::Duration::from_secs(1),
    };
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        daemon::serve(&serve_socket, options, move || {
            let _sent = ready_tx.send(());
        })
        .await
    });
    ready_rx.await.expect("daemon ready");
    assert!(daemon::ping(&socket).await);

    let params = ConnectParams {
        command: python.display().to_string(),
        args: vec![fixture()],
        cwd: dir.path().display().to_string(),
        env: Default::default(),
        initialization_options: None,
    };
    let mut link_results = Vec::new();
    for _ in 0..2 {
        let stream = tokio::net::UnixStream::connect(&socket)
            .await
            .expect("dial");
        let (read, mut write) = stream.into_split();
        let mut reader = BufReader::new(read);
        let send = |message: Value| message.to_string().into_bytes();
        write_frame(
            &mut write,
            &send(
                json!({"jsonrpc":"2.0","id":0,"method":CONNECT_METHOD,"params":params.to_json()}),
            ),
        )
        .await
        .expect("connect");
        let handshake: Value =
            serde_json::from_slice(&read_frame(&mut reader).await.expect("read").expect("frame"))
                .expect("json");
        write_frame(
            &mut write,
            &send(json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}})),
        )
        .await
        .expect("initialize");
        let init: Value =
            serde_json::from_slice(&read_frame(&mut reader).await.expect("read").expect("frame"))
                .expect("json");
        write_frame(
            &mut write,
            &send(json!({"jsonrpc":"2.0","id":2,"method":"shutdown"})),
        )
        .await
        .expect("shutdown");
        let shutdown: Value =
            serde_json::from_slice(&read_frame(&mut reader).await.expect("read").expect("frame"))
                .expect("json");
        assert_eq!(shutdown["id"], 2);
        write_frame(&mut write, &send(json!({"jsonrpc":"2.0","method":"exit"})))
            .await
            .expect("exit");
        drop(write);
        link_results.push((
            handshake["result"].clone(),
            init["result"]["serverInfo"]["version"].clone(),
        ));
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(link_results[0].0["spawned"], true);
    assert_eq!(link_results[1].0["spawned"], false);
    assert_eq!(link_results[0].0["pid"], link_results[1].0["pid"]);
    assert_eq!(link_results[0].0["key"], json!(params.key()));
    // The second initialize was answered from the cache: the server only initialized once.
    assert_eq!(link_results[0].1, link_results[1].1);
    assert!(
        link_results[0]
            .1
            .as_str()
            .is_some_and(|version| version.ends_with(":1"))
    );
    server.abort();
}
