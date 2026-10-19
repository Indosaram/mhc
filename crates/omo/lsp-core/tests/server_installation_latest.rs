use lsp_core::lsp::server_installation::{
    is_server_installed_with_options, reset_server_binary_resolution_cache_for_tests,
    resolve_server_binary, server_binary_probe_count_for_tests, ResolveServerBinaryOptions,
};
use std::fs::{create_dir_all, write};
use std::path::Path;
use std::sync::Mutex;
use tempfile::TempDir;

fn temp_root(prefix: &str) -> TempDir {
    tempfile::Builder::new().prefix(prefix).tempdir().expect("tempdir")
}

fn write_exec(path: &Path) -> String {
    if let Some(parent) = path.parent() {
        create_dir_all(parent).expect("mkdir");
    }
    write(path, "#!/bin/sh\nexit 0\n").expect("write");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    path.to_string_lossy().into_owned()
}

fn node_bin(root: &Path, name: &str) -> std::path::PathBuf {
    root.join("node_modules").join(".bin").join(name)
}

static TEST_CACHE_LOCK: Mutex<()> = Mutex::new(());

struct StateGuard { _lock: std::sync::MutexGuard<'static, ()> }

impl StateGuard {
    fn new() -> Self {
        let lock = TEST_CACHE_LOCK.lock().expect("cache test lock");
        reset_server_binary_resolution_cache_for_tests();
        Self { _lock: lock }
    }
}
impl Drop for StateGuard {
    fn drop(&mut self) {
        reset_server_binary_resolution_cache_for_tests();
    }
}

#[test]
fn test_install_node_marker_local_bin_with_empty_path() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-node-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let bin = write_exec(&node_bin(root.path(), "biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(
        &["biome".into(), "lsp-proxy".into(), "--stdio".into()],
        Some(&root.path().to_string_lossy()),
        &opts,
    );
    assert_eq!(resolved, Some(bin));
}

#[test]
fn test_install_nested_working_directory_walks_up_to_marker() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-nested-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let bin = write_exec(&node_bin(root.path(), "biome"));
    let nested = root.path().join("packages").join("deep").join("src");
    create_dir_all(&nested).expect("mkdir");
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(
        &["biome".into(), "lsp-proxy".into(), "--stdio".into()],
        Some(&nested.to_string_lossy()),
        &opts,
    );
    assert_eq!(resolved, Some(bin));
}

#[test]
fn test_install_local_bin_without_marker_not_trusted() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-unmarked-");
    write_exec(&node_bin(root.path(), "biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(
        &["biome".into(), "lsp-proxy".into(), "--stdio".into()],
        Some(&root.path().to_string_lossy()),
        &opts,
    );
    assert_eq!(resolved, None);
}

#[test]
fn test_install_bun_lock_probes_node_bin() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-bunlock-");
    write(root.path().join("bun.lock"), "").expect("write");
    let bin = write_exec(&node_bin(root.path(), "biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts);
    assert_eq!(resolved, Some(bin));
}

#[test]
fn test_install_pyproject_venv_binary() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-python-");
    write(root.path().join("pyproject.toml"), "[project]\n").expect("write");
    let bin = write_exec(&root.path().join(".venv").join("bin").join("ruff"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(&["ruff".into(), "server".into()], Some(&root.path().to_string_lossy()), &opts);
    assert_eq!(resolved, Some(bin));
}

#[test]
fn test_install_gemfile_bundled_binary() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-ruby-");
    write(root.path().join("Gemfile"), "source 'https://rubygems.org'\n").expect("write");
    let bin = write_exec(&root.path().join("vendor").join("bundle").join("bin").join("rubocop"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(&["rubocop".into(), "--lsp".into()], Some(&root.path().to_string_lossy()), &opts);
    assert_eq!(resolved, Some(bin));
}

#[test]
fn test_install_gomod_project_bin() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-go-");
    write(root.path().join("go.mod"), "module example.com/demo\n").expect("write");
    let bin = write_exec(&root.path().join("bin").join("gopls"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(&["gopls".into()], Some(&root.path().to_string_lossy()), &opts);
    assert_eq!(resolved, Some(bin));
}

#[test]
fn test_install_python_marker_does_not_borrow_node_bins() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-python-miss-");
    write(root.path().join("requirements.txt"), "ruff\n").expect("write");
    write_exec(&node_bin(root.path(), "ruff"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(&["ruff".into(), "server".into()], Some(&root.path().to_string_lossy()), &opts);
    assert_eq!(resolved, None);
}

#[test]
fn test_install_local_priority_over_path() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-local-priority-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let local_bin = write_exec(&node_bin(root.path(), "biome"));
    let path_dir = temp_root("lsp-path-priority-");
    write_exec(&path_dir.path().join("biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(path_dir.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts);
    assert_eq!(resolved, Some(local_bin));
}

#[test]
fn test_install_path_fallback() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-path-fallback-root-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let path_dir = temp_root("lsp-path-fallback-");
    let path_bin = write_exec(&path_dir.path().join("biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(path_dir.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    let resolved = resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts);
    assert_eq!(resolved, Some(path_bin));
}

#[test]
fn test_install_explicit_path_validation() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-explicit-path-");
    let bin = write_exec(&root.path().join("tools").join("custom-ls"));
    let opts = ResolveServerBinaryOptions::default();
    assert_eq!(
        resolve_server_binary(&[bin.clone(), "--stdio".into()], Some(&root.path().to_string_lossy()), &opts),
        Some(bin)
    );
    assert_eq!(
        resolve_server_binary(&[root.path().join("tools").join("absent-ls").to_string_lossy().into_owned()], Some(&root.path().to_string_lossy()), &opts),
        None
    );
}

#[test]
fn test_install_no_working_directory_falls_back_to_path() {
    let _guard = StateGuard::new();
    let path_dir = temp_root("lsp-no-cwd-");
    let path_bin = write_exec(&path_dir.path().join("biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(path_dir.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], None, &opts), Some(path_bin));
}

#[test]
fn test_install_nothing_anywhere_returns_none() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-unresolved-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts), None);
    assert_eq!(resolve_server_binary(&[], Some(&root.path().to_string_lossy()), &opts), None);
}

#[test]
fn test_install_boundary_containment() {
    let _guard = StateGuard::new();
    let outer = temp_root("lsp-boundary-outer-");
    write(outer.path().join("package.json"), "{}\n").expect("write");
    let bin = write_exec(&node_bin(outer.path(), "biome"));
    let inner = outer.path().join("inner");
    create_dir_all(&inner).expect("mkdir");
    let opts = ResolveServerBinaryOptions {
        stop_at: Some(inner.to_string_lossy().into_owned()),
        path_env: Some(outer.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&inner.to_string_lossy()), &opts), None);
    let unbounded = ResolveServerBinaryOptions {
        path_env: Some(outer.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&inner.to_string_lossy()), &unbounded), Some(bin));
}

#[test]
fn test_install_git_boundary_stops_walk() {
    let _guard = StateGuard::new();
    let outer = temp_root("lsp-git-boundary-");
    write(outer.path().join("package.json"), "{}\n").expect("write");
    write_exec(&node_bin(outer.path(), "biome"));
    let repo = outer.path().join("repo");
    create_dir_all(repo.join(".git")).expect("mkdir");
    let opts = ResolveServerBinaryOptions {
        path_env: Some(outer.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&repo.to_string_lossy()), &opts), None);
}

#[test]
fn test_install_windows_suffix_cmd_local() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-win-local-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let bin = write_exec(&node_bin(root.path(), "biome.cmd"));
    let opts = ResolveServerBinaryOptions {
        platform: Some("win32"),
        path_ext: Some(".COM;.EXE;.CMD".into()),
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts), Some(bin));
}

#[test]
fn test_install_windows_suffix_exe_path() {
    let _guard = StateGuard::new();
    let path_dir = temp_root("lsp-win-path-");
    let bin = write_exec(&path_dir.path().join("biome.exe"));
    let opts = ResolveServerBinaryOptions {
        platform: Some("win32"),
        path_ext: Some(".COM;.EXE".into()),
        path_env: Some(path_dir.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], None, &opts), Some(bin));
}

#[test]
fn test_install_caching_hit() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-cache-hit-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let bin = write_exec(&node_bin(root.path(), "biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    let first = resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts);
    let count1 = server_binary_probe_count_for_tests();
    let second = resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts);
    let count2 = server_binary_probe_count_for_tests();
    assert_eq!(first, Some(bin.clone()));
    assert_eq!(second, Some(bin));
    assert!(count1 > 0);
    assert_eq!(count1, count2);
}

#[test]
fn test_install_caching_miss() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-cache-miss-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts), None);
    let count1 = server_binary_probe_count_for_tests();
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&root.path().to_string_lossy()), &opts), None);
    assert_eq!(server_binary_probe_count_for_tests(), count1);
}

#[test]
fn test_install_distinct_working_directories_separate_cache() {
    let _guard = StateGuard::new();
    let first = temp_root("lsp-cache-key-a-");
    write(first.path().join("package.json"), "{}\n").expect("write");
    let bin1 = write_exec(&node_bin(first.path(), "biome"));
    let second = temp_root("lsp-cache-key-b-");
    write(second.path().join("package.json"), "{}\n").expect("write");
    let opts = ResolveServerBinaryOptions {
        path_env: Some(first.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&first.path().to_string_lossy()), &opts), Some(bin1));
    assert_eq!(resolve_server_binary(&["biome".into()], Some(&second.path().to_string_lossy()), &opts), None);
}

#[test]
fn test_is_server_installed_with_options_compat_cases() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-compat-");
    write(root.path().join("package.json"), "{}\n").expect("write");
    write_exec(&node_bin(root.path(), "biome"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(is_server_installed_with_options(&["biome".into(), "lsp-proxy".into()], Some(&root.path().to_string_lossy()), &opts));
    assert!(!is_server_installed_with_options(&["totally-absent-ls".into()], Some(&root.path().to_string_lossy()), &opts));
    assert!(!is_server_installed_with_options(&[], Some(&root.path().to_string_lossy()), &opts));
    assert!(is_server_installed_with_options(&["node".into(), "server.js".into()], Some(&root.path().to_string_lossy()), &opts));
}

#[test]
fn installation_check_uses_path_without_working_directory() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-install-path-");
    write_exec(&root.path().join("biome"));
    let options = ResolveServerBinaryOptions {
        path_env: Some(root.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(is_server_installed_with_options(&["biome".into()], None, &options));
}

#[test]
fn installation_check_reports_absent_binary() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-install-absent-");
    let options = ResolveServerBinaryOptions {
        path_env: Some(root.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(!is_server_installed_with_options(&["absent-server".into()], Some(&root.path().to_string_lossy()), &options));
}

#[test]
fn installation_check_preserves_node_allowance() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-install-node-");
    let options = ResolveServerBinaryOptions {
        path_env: Some(root.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert!(is_server_installed_with_options(&["node".into(), "server.js".into()], Some(&root.path().to_string_lossy()), &options));
}

#[test]
fn test_install_node_fallback_behavior() {
    let _guard = StateGuard::new();
    let root = temp_root("lsp-node-fallback-");
    let opts = ResolveServerBinaryOptions {
        path_env: Some(root.path().join("definitely-empty").to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["node".into(), "server.js".into()], Some(&root.path().to_string_lossy()), &opts), Some("node".into()));
}

#[test]
fn test_install_node_on_path_wins() {
    let _guard = StateGuard::new();
    let path_dir = temp_root("lsp-node-on-path-");
    let path_bin = write_exec(&path_dir.path().join("node"));
    let opts = ResolveServerBinaryOptions {
        path_env: Some(path_dir.path().to_string_lossy().into_owned()),
        ..Default::default()
    };
    assert_eq!(resolve_server_binary(&["node".into(), "server.js".into()], None, &opts), Some(path_bin));
}
