use maho_machine_ops::files::local_files::LocalFileOps;
use maho_machine_ops::{
    AbortSignal, FileOps, GlobRequest, Limits, OpContext, OpPrincipal, OpScope, PolicyHooks,
    PrincipalKind,
};
use std::path::PathBuf;

#[tokio::test]
async fn rooted_glob_ending_in_separator_lists_directory_like_star_on_walker() {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = tokio::fs::canonicalize(temp.path())
        .await
        .expect("canonicalize")
        .join("root");
    tokio::fs::create_dir_all(root.join("dir"))
        .await
        .expect("mkdir dir");
    tokio::fs::write(root.join("dir/a.ts"), "a")
        .await
        .expect("write a.ts");
    tokio::fs::write(root.join("dir/b.ts"), "b")
        .await
        .expect("write b.ts");

    let files = LocalFileOps::new_walking();
    let ctx = OpContext {
        scope: OpScope {
            id: "s".to_owned(),
            root: Some(root.clone()),
        },
        principal: OpPrincipal {
            kind: PrincipalKind::LocalSession,
            id: "p".to_owned(),
        },
        signal: AbortSignal::default(),
        limits: Limits {
            read_max_bytes: Some(1 << 20),
            exec_stdout_max_bytes: 0,
            exec_stderr_max_bytes: 0,
            exec_timeout_default_ms: Some(60_000),
            ..Limits::default()
        },
        policy: PolicyHooks::default(),
        audit: None,
        path_lock: None,
    };

    let run = |pattern: String, cwd: Option<String>| async {
        let res = files
            .glob(
                GlobRequest {
                    pattern,
                    cwd,
                    limit: None,
                },
                &ctx,
            )
            .await
            .expect("glob");
        let mut paths: Vec<PathBuf> = res.entries.into_iter().map(|e| e.path).collect();
        paths.sort();
        paths
    };

    let dir_str = root.join("dir").to_string_lossy().into_owned();
    let star = run("*".to_owned(), Some(dir_str.clone())).await;
    assert_eq!(star, vec![root.join("dir/a.ts"), root.join("dir/b.ts")]);
    assert_eq!(run(format!("{dir_str}/"), None).await, star);
    assert_eq!(run("../dir/".to_owned(), Some(dir_str)).await, star);
}
