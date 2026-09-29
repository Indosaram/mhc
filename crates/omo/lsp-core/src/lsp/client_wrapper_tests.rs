use super::*;
use crate::lsp::manager::LspManagerOptions;
use crate::request_context::StandaloneMcpRequestContextInput;
use crate::request_context::create_standalone_mcp_request_context;
use crate::request_context::run_with_request_context;
use crate::request_context::scope_request_context;
use std::path::PathBuf;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

fn temp_root() -> tempfile::TempDir {
    tempfile::tempdir().expect("tempdir")
}

fn real(path: &Path) -> String {
    std::fs::canonicalize(path)
        .expect("realpath")
        .to_string_lossy()
        .into_owned()
}

fn context_for(root: &Path) -> crate::request_context::LspRequestContext {
    create_standalone_mcp_request_context(StandaloneMcpRequestContextInput {
        cwd: Some(root.to_string_lossy().into_owned()),
        env: Some(Default::default()),
        home_dir: None,
    })
    .expect("context")
}

fn joined(root: &str, parts: &[&str]) -> String {
    let mut path = PathBuf::from(root);
    path.extend(parts);
    path.to_string_lossy().into_owned()
}

#[test]
fn relative_file_inside_cwd_resolves_marker_workspace_inside_cwd() {
    let root = temp_root();
    std::fs::create_dir_all(root.path().join(".git")).expect("mkdir");
    std::fs::create_dir_all(root.path().join("src")).expect("mkdir");
    std::fs::write(root.path().join("src/file.ts"), "export const value = 1;\n").expect("write");

    let workspace = run_with_request_context(context_for(root.path()), || {
        find_workspace_root("src/file.ts")
    });

    assert_eq!(workspace.expect("workspace"), real(root.path()));
}

#[test]
fn absolute_file_outside_cwd_rejects_before_workspace_inference() {
    let root = temp_root();
    let outside = temp_root();
    std::fs::create_dir_all(outside.path().join(".git")).expect("mkdir");
    std::fs::write(
        outside.path().join("file.ts"),
        "export const outside = true;\n",
    )
    .expect("write");
    let target = outside
        .path()
        .join("file.ts")
        .to_string_lossy()
        .into_owned();

    let result =
        run_with_request_context(context_for(root.path()), || find_workspace_root(&target));

    assert!(
        matches!(result, Err(LspError::InvalidPath(_))),
        "{result:?}"
    );
}

#[test]
fn symlink_inside_cwd_pointing_outside_keeps_lexical_read_path_and_cwd_root() {
    let root = temp_root();
    let outside = temp_root();
    std::fs::create_dir_all(root.path().join(".git")).expect("mkdir");
    std::fs::create_dir_all(outside.path().join(".git")).expect("mkdir");
    std::fs::write(
        outside.path().join("file.ts"),
        "export const outside = true;\n",
    )
    .expect("write");
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).expect("symlink");

    let (file_path, workspace_root) = run_with_request_context(context_for(root.path()), || {
        (
            resolve_readable_path_inside_context("linked/file.ts"),
            find_workspace_root("linked/file.ts"),
        )
    });

    assert_eq!(
        file_path.expect("path"),
        joined(&real(root.path()), &["linked", "file.ts"])
    );
    assert_eq!(workspace_root.expect("workspace"), real(root.path()));
}

#[test]
fn absolute_alias_path_through_outside_symlink_preserves_lexical_suffix() {
    let root = temp_root();
    let outside = temp_root();
    std::fs::create_dir_all(root.path().join(".git")).expect("mkdir");
    std::fs::write(
        outside.path().join("file.ts"),
        "export const outside = true;\n",
    )
    .expect("write");
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).expect("symlink");
    let alias = root
        .path()
        .join("linked")
        .join("file.ts")
        .to_string_lossy()
        .into_owned();

    let result = run_with_request_context(context_for(root.path()), || {
        resolve_readable_path_inside_context(&alias)
    });

    assert_eq!(
        result.expect("path"),
        joined(&real(root.path()), &["linked", "file.ts"])
    );
}

#[test]
fn missing_parent_directories_without_markers_use_existing_directory() {
    let root = temp_root();

    let workspace = run_with_request_context(context_for(root.path()), || {
        find_workspace_root("missing/deep/file.ts")
    });

    assert_eq!(workspace.expect("workspace"), real(root.path()));
}

#[tokio::test]
async fn rename_through_outside_symlink_rejects_before_client_acquisition() {
    let root = temp_root();
    let outside = temp_root();
    std::fs::write(
        outside.path().join("file.ts"),
        "export const outside = true;\n",
    )
    .expect("write");
    std::os::unix::fs::symlink(outside.path(), root.path().join("linked")).expect("symlink");
    let factory_calls = Arc::new(AtomicUsize::new(0));
    let calls = factory_calls.clone();
    let manager = LspManager::new(LspManagerOptions {
        client_factory: Some(Arc::new(move |_, _| {
            calls.fetch_add(1, Ordering::SeqCst);
            Err(LspError::other("client acquisition must not start"))
        })),
        ..LspManagerOptions::default()
    });

    let result = scope_request_context(
        context_for(root.path()),
        with_lsp_client(
            "linked/file.ts",
            |_, _, _| async { Ok(()) },
            "rename",
            WithLspClientOptions {
                signal: None,
                manager: Some(manager.clone()),
            },
        ),
    )
    .await;
    manager.stop_all().await;

    assert!(
        matches!(result, Err(LspError::InvalidPath(_))),
        "{result:?}"
    );
    assert_eq!(factory_calls.load(Ordering::SeqCst), 0);
}
