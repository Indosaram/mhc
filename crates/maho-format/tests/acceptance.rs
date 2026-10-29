//! Acceptance tests of docs/cleanroom/specs/post-edit-format.md §8 (numbered `atNN_*`).
//! Tests that spawn fake formatter shell scripts are unix-only.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]
use maho_format::detect::{FormatterKind, Markers, is_bun_project};
use maho_format::engine::{FormatEngine, FormatReport, FormatRequest};
use maho_format::lsp::{LspAnswer, LspFormat, NoLsp};
use maho_format::paths::{collect_paths, resolve};
use maho_format::run::{Host, RunOutcome, SharedRuns, line_delta};
use maho_format::settings::{Mode, Settings};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("mkdir");
    }
    std::fs::write(path, text).expect("write");
}

/// A project directory plus an empty formatter bin directory (the only binary search path).
struct Project {
    root: tempfile::TempDir,
    bin: tempfile::TempDir,
}

impl Project {
    fn new() -> Self {
        Self {
            root: tempfile::tempdir().expect("tempdir"),
            bin: tempfile::tempdir().expect("tempdir"),
        }
    }
    fn cwd(&self) -> &Path {
        self.root.path()
    }
    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.cwd().join(name);
        write(&path, text);
        path
    }
    fn host(&self) -> Arc<Host> {
        Arc::new(Host::new(Some(vec![self.bin.path().to_path_buf()]), None))
    }
    #[cfg(unix)]
    fn tool(&self, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let path = self.bin.path().join(name);
        write(&path, &format!("#!/bin/sh\n{body}\n"));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    fn request(&self, tool: &str, input: Value, settings: Settings) -> FormatRequest {
        FormatRequest {
            tool_name: tool.to_owned(),
            input,
            details: None,
            cwd: self.cwd().to_path_buf(),
            session_id: "s1".to_owned(),
            settings: Some(settings),
        }
    }
}

/// Appends two lines to the last argument (the file).
#[cfg(unix)]
const ADD_TWO: &str = "for f; do last=$f; done; printf 'one\\ntwo\\n' >> \"$last\"";

fn engine(host: Arc<Host>) -> FormatEngine {
    FormatEngine::new(host, Arc::new(NoLsp))
}

fn settings(mode: Mode) -> Settings {
    Settings {
        mode,
        ..Settings::default()
    }
}

fn single_block(report: &FormatReport) -> &str {
    assert_eq!(report.blocks.len(), 1, "{report:?}");
    &report.blocks[0]
}

#[tokio::test]
async fn at01_non_mutation_tools_are_ignored() {
    let project = Project::new();
    project.file("biome.json", "{}");
    project.file("a.ts", "x");
    let host = project.host();
    let engine = engine(Arc::clone(&host));
    for tool in ["read", "bash"] {
        let report = engine
            .process(&project.request(tool, json!({"path": "a.ts"}), Settings::default()))
            .await;
        assert_eq!(report, FormatReport::default());
    }
    assert_eq!(host.processes(), 0);
}

#[tokio::test]
async fn at02_mode_off_runs_nothing() {
    let project = Project::new();
    project.file("biome.json", "{}");
    project.file("a.ts", "x");
    let host = project.host();
    let report = engine(Arc::clone(&host))
        .process(&project.request("write", json!({"path": "a.ts"}), settings(Mode::Off)))
        .await;
    assert_eq!(report, FormatReport::default());
    assert_eq!(host.processes(), 0);
}

#[test]
fn at03_path_keys_keep_spaces_drop_empty_and_dedupe() {
    let input = json!({
        "path": " a.ts ",
        "filePath": "",
        "paths": ["b.ts", 3, " a.ts "],
        "filePaths": ["b.ts", "c.ts", null]
    });
    assert_eq!(collect_paths("edit", &input, None), [" a.ts ", "b.ts", "c.ts"]);
}

#[test]
fn at04_patch_headers_and_change_objects() {
    let input = json!({
        "input": "*** Begin Patch\n*** Update File:  lib/x.py \n@@\n-a\n+b\n*** Add File: y.md\n+hi\n*** End Patch",
        "changes": [{"path": "z.go", "movePath": "w/z.go"}]
    });
    assert_eq!(
        collect_paths("apply_patch", &input, None),
        ["lib/x.py", "y.md", "z.go", "w/z.go"]
    );
    let details = json!({"result": {"appliedFiles": ["y.md", "q.rs"]}});
    assert_eq!(
        collect_paths("apply_patch", &input, Some(&details)),
        ["lib/x.py", "y.md", "q.rs", "z.go", "w/z.go"]
    );
}

#[test]
fn at05_paths_are_normalized_lexically() {
    let cwd = Path::new("/work/proj");
    assert_eq!(resolve(cwd, "docs/../docs/r.md"), cwd.join("docs/r.md"));
    assert_eq!(resolve(cwd, "./pkg/./m.go"), cwd.join("pkg/m.go"));
    #[cfg(unix)]
    assert_eq!(resolve(cwd, "/abs/./x/../y.rs"), PathBuf::from("/abs/y.rs"));
}

fn chosen(markers: &[(&str, &str)], file: &str) -> Option<FormatterKind> {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text) in markers {
        write(&dir.path().join(name), text);
    }
    Markers::scan(dir.path()).choose(Path::new(file))
}

#[test]
fn at06_formatter_choice_from_markers() {
    assert_eq!(
        chosen(&[("biome.jsonc", "{}"), ("prettier.config.mjs", "")], "a.ts"),
        Some(FormatterKind::Biome)
    );
    assert_eq!(
        chosen(&[(".prettierrc.yaml", "")], "a.css"),
        Some(FormatterKind::Prettier)
    );
    assert_eq!(chosen(&[("rustfmt.toml", "")], "m.rs"), Some(FormatterKind::Rustfmt));
    assert_eq!(chosen(&[("go.mod", "module m")], "m.go"), Some(FormatterKind::Gofmt));
    assert_eq!(
        chosen(&[("pyproject.toml", "[project]\n  [tool.ruff]\n")], "m.py"),
        Some(FormatterKind::Ruff)
    );
    assert_eq!(
        chosen(&[("pyproject.toml", "[project]\n[tool.isort]\n")], "m.py"),
        None
    );
}

#[tokio::test]
async fn at07_wrong_extension_or_no_markers_formats_nothing() {
    assert_eq!(chosen(&[("Cargo.toml", "")], "m.go"), None);
    assert_eq!(chosen(&[], "m.go"), None);
    let project = Project::new();
    project.file("m.go", "package m\n");
    let host = project.host();
    let report = engine(Arc::clone(&host))
        .process(&project.request("write", json!({"path": "m.go"}), settings(Mode::Required)))
        .await;
    assert_eq!(report, FormatReport::default());
    assert_eq!(host.formatter_runs(), 0);
}

#[cfg(unix)]
#[tokio::test]
async fn at08_large_and_git_ignored_files_are_skipped() {
    let project = Project::new();
    project.tool("gofmt", ADD_TWO);
    project.file("go.mod", "module m\n");
    project.file("big.go", &"x".repeat(64));
    project.file(".gitignore", "gen.go\n");
    project.file("gen.go", "package m\n");
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(project.cwd())
        .status()
        .expect("git init");
    assert!(status.success());
    let host = project.host();
    let mut config = settings(Mode::Required);
    config.max_file_bytes = 16;
    let report = engine(Arc::clone(&host))
        .process(&project.request(
            "write",
            json!({"paths": ["big.go", "gen.go"]}),
            config,
        ))
        .await;
    assert_eq!(report, FormatReport::default());
    assert_eq!(host.formatter_runs(), 0);
}

#[tokio::test]
async fn at09_language_off_wins_over_required() {
    let project = Project::new();
    project.file("biome.json", "{}");
    project.file("m.py", "x = 1\n");
    project.file("a.ts", "let a\n");
    let mut config = settings(Mode::Required);
    config.languages.insert("python".into(), false);
    assert_eq!(config.mode_for("python"), Mode::Off);
    assert_eq!(config.mode_for("typescript"), Mode::Required);
    let host = project.host();
    let report = engine(Arc::clone(&host))
        .process(&project.request("write", json!({"paths": ["m.py", "a.ts"]}), config))
        .await;
    assert!(report.is_error);
    let block = single_block(&report);
    assert!(block.contains("a.ts") && block.contains("biome"), "{block}");
    assert!(!block.contains("m.py"));
}

#[test]
fn at10_config_parsing_and_defaults() {
    let parsed = Settings::from_section(Some(&json!({
        "mode": "off", "languages": {"rs": false, "go": true},
        "maxFileBytes": 2048, "timeoutMs": 750
    })));
    assert_eq!(parsed.mode, Mode::Off);
    assert_eq!(parsed.languages.get("rs"), Some(&false));
    assert_eq!(parsed.languages.get("go"), Some(&true));
    assert_eq!(parsed.max_file_bytes, 2048);
    assert_eq!(parsed.timeout, Duration::from_millis(750));
    let snake = Settings::from_section(Some(&json!({"max_file_bytes": 10, "timeout_ms": 20})));
    assert_eq!(snake.max_file_bytes, 10);
    assert_eq!(snake.timeout, Duration::from_millis(20));
    for absent in [None, Some(json!("nope")), Some(json!({"mode": "loud"}))] {
        let defaults = Settings::from_section(absent.as_ref());
        assert_eq!(defaults.mode, Mode::BestEffort);
        assert_eq!(defaults.max_file_bytes, 1_048_576);
        assert_eq!(defaults.timeout, Duration::from_millis(3000));
    }
    let view = json!({"formatOnMutation": {"mode": "required"}});
    assert_eq!(Settings::from_view(&view).mode, Mode::Required);
}

#[cfg(unix)]
#[tokio::test]
async fn at11_formatted_file_gets_one_block() {
    let project = Project::new();
    project.tool("gofmt", ADD_TWO);
    project.file("go.mod", "module m\n");
    project.file("pkg/m.go", "package m\n");
    let report = engine(project.host())
        .process(&project.request("write", json!({"path": "pkg/m.go"}), Settings::default()))
        .await;
    let block = single_block(&report);
    assert!(block.contains("pkg/m.go"), "{block}");
    assert!(block.contains("gofmt"), "{block}");
    assert!(block.contains("+2/-0"), "{block}");
    assert!(!report.is_error);
}

#[cfg(unix)]
async fn run_failing_tool(body: &str, timeout: Duration) -> (FormatReport, Vec<u8>, Vec<u8>) {
    let project = Project::new();
    project.tool("gofmt", body);
    project.file("go.mod", "module m\n");
    let file = project.file("m.go", "package m\n");
    let original = std::fs::read(&file).expect("read");
    let config = Settings { timeout, ..Settings::default() };
    let report = engine(project.host())
        .process(&project.request("edit", json!({"path": "m.go"}), config))
        .await;
    (report, original, std::fs::read(&file).expect("read"))
}

#[cfg(unix)]
#[tokio::test]
async fn at12_nonzero_exit_restores_the_file() {
    let (report, original, after) =
        run_failing_tool(&format!("{ADD_TWO}; exit 3"), Duration::from_secs(10)).await;
    assert_eq!(after, original);
    assert_eq!(report, FormatReport::default());
}

#[cfg(unix)]
#[tokio::test]
async fn at13_timeout_kills_and_restores() {
    let started = std::time::Instant::now();
    let (report, original, after) =
        run_failing_tool(&format!("{ADD_TWO}; exec sleep 30"), Duration::from_millis(300)).await;
    assert!(started.elapsed() < Duration::from_secs(20));
    assert_eq!(after, original);
    assert_eq!(report, FormatReport::default());
}

#[cfg(unix)]
#[tokio::test]
async fn at14_untouched_file_gives_no_block() {
    let (report, original, after) = run_failing_tool("exit 0", Duration::from_secs(10)).await;
    assert_eq!(after, original);
    assert_eq!(report, FormatReport::default());
}

#[test]
fn at15_line_counts() {
    assert_eq!(line_delta(b"x\ny\n", b"x\n"), (0, 1));
    assert_eq!(line_delta(b"p", b"p\nq\nr"), (2, 0));
}

#[tokio::test]
async fn at16_missing_best_effort_warns_once_per_session() {
    let project = Project::new();
    project.file("go.mod", "module m\n");
    project.file("m.go", "package m\n");
    project.file("n.go", "package m\n");
    let engine = engine(project.host());
    let mut warnings = Vec::new();
    for (session, file) in [("s1", "m.go"), ("s1", "n.go"), ("s2", "m.go")] {
        let mut request = project.request("write", json!({"path": file}), Settings::default());
        request.session_id = session.to_owned();
        let report = engine.process(&request).await;
        assert!(!report.is_error);
        assert!(report.blocks.is_empty());
        warnings.push(report.warnings);
    }
    assert_eq!(warnings[0].len(), 1);
    assert!(warnings[0][0].contains("gofmt"));
    assert!(warnings[1].is_empty());
    assert_eq!(warnings[2].len(), 1);
}

#[tokio::test]
async fn at17_missing_required_marks_error() {
    let project = Project::new();
    project.file("go.mod", "module m\n");
    project.file("src/m.go", "package m\n");
    let report = engine(project.host())
        .process(&project.request(
            "write",
            json!({"path": "./src/m.go"}),
            settings(Mode::Required),
        ))
        .await;
    assert!(report.is_error);
    let block = single_block(&report);
    assert!(block.contains("gofmt") && block.contains("./src/m.go"), "{block}");
}

struct FakeLsp(LspAnswer);

impl LspFormat for FakeLsp {
    fn format<'a>(
        &'a self,
        _cwd: &'a Path,
        _path: &'a Path,
    ) -> maho_lsp::hooks::HookFuture<'a, LspAnswer> {
        let answer = self.0.clone();
        Box::pin(async move { answer })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn at18_lsp_path() {
    let cases = [
        (LspAnswer::Formatted { added: 4, removed: 3 }, Some("+4/-3"), 0),
        (LspAnswer::Unavailable, Some("+2/-0"), 1),
        (LspAnswer::Other, None, 0),
    ];
    for (answer, token, cli_runs) in cases {
        let project = Project::new();
        project.tool("gofmt", ADD_TWO);
        project.file("go.mod", "module m\n");
        project.file("m.go", "package m\n");
        let host = project.host();
        let engine = FormatEngine::new(Arc::clone(&host), Arc::new(FakeLsp(answer)));
        let report = engine
            .process(&project.request("write", json!({"path": "m.go"}), Settings::default()))
            .await;
        match token {
            Some(token) => assert!(single_block(&report).contains(token), "{report:?}"),
            None => assert!(report.blocks.is_empty(), "{report:?}"),
        }
        assert_eq!(host.formatter_runs(), cli_runs);
    }
}

#[tokio::test]
async fn at19_concurrent_runs_share_and_cancel_releases() {
    let runs = SharedRuns::default();
    let path = Path::new("/w/m.go");
    let calls = AtomicUsize::new(0);
    let gate = tokio::sync::Notify::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel::<()>();
    let started_tx = std::sync::Mutex::new(Some(started_tx));
    let (calls, gate, started_tx) = (&calls, &gate, &started_tx);
    let work = move || async move {
        calls.fetch_add(1, Ordering::SeqCst);
        if let Some(tx) = started_tx.lock().expect("lock").take() {
            let _ = tx.send(());
        }
        gate.notified().await;
        RunOutcome::Formatted {
            formatter: "gofmt".into(),
            added: 1,
            removed: 0,
        }
    };
    let release = async {
        started_rx.await.expect("started");
        gate.notify_one();
    };
    let bounded = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::join!(runs.run(path, work()), runs.run(path, work()), release)
    });
    let (first, second, ()) = bounded.await.expect("shared run finished");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(first, second);
    assert!(matches!(first, RunOutcome::Formatted { added: 1, .. }));
    assert_eq!(runs.pending(), 0);

    let cancelled = runs.run(path, std::future::pending::<RunOutcome>());
    tokio::select! {
        biased;
        _ = cancelled => panic!("pending run finished"),
        () = std::future::ready(()) => {}
    }
    let next = tokio::time::timeout(
        Duration::from_secs(10),
        runs.run(path, async { RunOutcome::Unchanged }),
    )
    .await
    .expect("not blocked by the cancelled run");
    assert_eq!(next, RunOutcome::Unchanged);
}

#[cfg(unix)]
async fn bun_case(setup: impl Fn(&Project), bun: &str) -> String {
    let project = Project::new();
    project.file(".prettierrc", "{}");
    project.file("bunfig.toml", "");
    project.file("a.ts", "let a\n");
    project.tool("bun", bun);
    project.tool("prettier", ADD_TWO);
    setup(&project);
    let report = engine(project.host())
        .process(&project.request("write", json!({"path": "a.ts"}), Settings::default()))
        .await;
    single_block(&report).to_owned()
}

#[cfg(unix)]
#[tokio::test]
async fn at20_bun_substitution() {
    let good = format!("if [ \"$2\" = \"--help\" ]; then exit 0; fi\n{ADD_TWO}");
    let lazy = "exit 0";
    let block = bun_case(|_| {}, &good).await;
    assert!(block.contains("bun format"), "{block}");
    let block = bun_case(
        |project| {
            project.file("package.json", r#"{"scripts":{"format":"x"}}"#);
        },
        &good,
    )
    .await;
    assert!(block.contains("prettier") && !block.contains("bun format"), "{block}");
    let block = bun_case(
        |project| {
            project.file("format/package.json", "{}");
            project.file("format/index.js", "");
        },
        &good,
    )
    .await;
    assert!(block.contains("prettier") && !block.contains("bun format"), "{block}");
    let block = bun_case(|_| {}, lazy).await;
    assert!(block.contains("prettier") && !block.contains("bun format"), "{block}");
}

#[test]
fn at21_bun_detection_stops_at_git_and_home() {
    let dir = tempfile::tempdir().expect("tempdir");
    write(&dir.path().join("bun.lock"), "");
    let repo = dir.path().join("repo");
    let cwd = repo.join("pkg");
    std::fs::create_dir_all(repo.join(".git")).expect("mkdir");
    std::fs::create_dir_all(&cwd).expect("mkdir");
    assert!(!is_bun_project(&cwd, None));
    let home = dir.path().join("home");
    let inside = home.join("proj");
    std::fs::create_dir_all(&inside).expect("mkdir");
    assert!(!is_bun_project(&inside, Some(&home)));
    assert!(is_bun_project(&inside, None));
}
