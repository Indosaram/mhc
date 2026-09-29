use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use tempfile::tempdir;
use utils::codegraph::*;

#[test]
fn resolve_command_prefers_omo_codegraph_bin() {
    let mut env = BTreeMap::new();
    env.insert(
        "OMO_CODEGRAPH_BIN".to_string(),
        "/opt/codegraph/bin/codegraph".to_string(),
    );

    let file_exists = |p: &Path| p == Path::new("/opt/codegraph/bin/codegraph");
    let provisioned = || Some(PathBuf::from("/provisioned/codegraph"));
    let req_resolve = |_spec: &str| Some(PathBuf::from("/bundle/package.json"));
    let which = |_cmd: &str| Some(PathBuf::from("/usr/local/bin/codegraph"));

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        env: Some(env),
        file_exists: Some(&file_exists),
        provisioned: Some(&provisioned),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: "/opt/codegraph/bin/codegraph".to_string(),
            exists: true,
            source: CodegraphCommandSource::Env,
        }
    );
}

#[test]
fn resolve_command_invalid_omo_codegraph_bin_unavailable() {
    let mut env = BTreeMap::new();
    env.insert("OMO_CODEGRAPH_BIN".to_string(), "/nonexistent".to_string());

    let file_exists = |_p: &Path| false;
    let provisioned = || Some(PathBuf::from("/provisioned/codegraph"));
    let req_resolve = |_spec: &str| Some(PathBuf::from("/bundle/package.json"));
    let which = |_cmd: &str| Some(PathBuf::from("/usr/local/bin/codegraph"));

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        env: Some(env),
        file_exists: Some(&file_exists),
        provisioned: Some(&provisioned),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: "/nonexistent".to_string(),
            exists: false,
            source: CodegraphCommandSource::Env,
        }
    );
}

#[test]
fn resolve_command_invalid_codegraph_bin_unavailable() {
    let mut env = BTreeMap::new();
    env.insert(
        "CODEGRAPH_BIN".to_string(),
        "/missing-codegraph".to_string(),
    );

    let file_exists = |_p: &Path| false;
    let provisioned = || Some(PathBuf::from("/provisioned/codegraph"));
    let req_resolve = |_spec: &str| Some(PathBuf::from("/bundle/package.json"));
    let which = |_cmd: &str| Some(PathBuf::from("/usr/local/bin/codegraph"));

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        env: Some(env),
        file_exists: Some(&file_exists),
        provisioned: Some(&provisioned),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: "/missing-codegraph".to_string(),
            exists: false,
            source: CodegraphCommandSource::Env,
        }
    );
}

#[test]
fn resolve_command_bundled_package() {
    let package_root = PathBuf::from("/bundle/node_modules/@colbymchenry/codegraph");
    let bundled_shim = package_root.join("bin").join("codegraph.js");
    let package_json = package_root.join("package.json");

    let file_exists = move |p: &Path| p == bundled_shim;
    let node_runtime = || Some("/usr/local/bin/node".to_string());
    let provisioned = || None;
    let req_resolve = move |_spec: &str| Some(package_json.clone());
    let which = |_cmd: &str| Some(PathBuf::from("/usr/local/bin/codegraph"));

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        file_exists: Some(&file_exists),
        node_runtime: Some(&node_runtime),
        provisioned: Some(&provisioned),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![
                "/bundle/node_modules/@colbymchenry/codegraph/bin/codegraph.js".to_string()
            ],
            command: "/usr/local/bin/node".to_string(),
            exists: true,
            source: CodegraphCommandSource::Bundled,
        }
    );
}

#[test]
fn resolve_command_bundled_with_codegraph_node_bin_override() {
    let package_root = PathBuf::from("/bundle/node_modules/@colbymchenry/codegraph");
    let bundled_shim = package_root.join("bin").join("codegraph.js");
    let package_json = package_root.join("package.json");
    let node_bin = PathBuf::from("/opt/node22/bin/node");

    let mut env = BTreeMap::new();
    env.insert(
        "CODEGRAPH_NODE_BIN".to_string(),
        node_bin.to_string_lossy().into_owned(),
    );

    let file_exists = move |p: &Path| p == bundled_shim || p == node_bin;
    let node_ver = |candidate: &str| {
        if candidate == "/opt/node22/bin/node" {
            Some("v22.22.3".to_string())
        } else {
            Some("v26.3.0".to_string())
        }
    };
    let provisioned = || None;
    let req_resolve = move |_spec: &str| Some(package_json.clone());
    let which = |_cmd: &str| None;

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        env: Some(env),
        file_exists: Some(&file_exists),
        node_version: Some(&node_ver),
        provisioned: Some(&provisioned),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![
                "/bundle/node_modules/@colbymchenry/codegraph/bin/codegraph.js".to_string()
            ],
            command: "/opt/node22/bin/node".to_string(),
            exists: true,
            source: CodegraphCommandSource::Bundled,
        }
    );
}

#[test]
fn resolve_command_bundled_selects_node22_on_path() {
    let package_root = PathBuf::from("/bundle/node_modules/@colbymchenry/codegraph");
    let bundled_shim = package_root.join("npm-shim.js");
    let package_json = package_root.join("package.json");
    let node22 = PathBuf::from("/usr/local/bin/node22");

    let file_exists = move |p: &Path| p == bundled_shim || p == node22;
    let node_ver = |candidate: &str| {
        if candidate == "/usr/local/bin/node22" {
            Some("22.14.0".to_string())
        } else {
            Some("26.3.0".to_string())
        }
    };
    let provisioned = || None;
    let req_resolve = move |_spec: &str| Some(package_json.clone());
    let which = |cmd: &str| {
        if cmd == "node22" {
            Some(PathBuf::from("/usr/local/bin/node22"))
        } else {
            None
        }
    };

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        file_exists: Some(&file_exists),
        node_version: Some(&node_ver),
        provisioned: Some(&provisioned),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![
                "/bundle/node_modules/@colbymchenry/codegraph/npm-shim.js".to_string()
            ],
            command: "/usr/local/bin/node22".to_string(),
            exists: true,
            source: CodegraphCommandSource::Bundled,
        }
    );
}

#[test]
fn resolve_node_support_with_node22_on_path() {
    let node22 = "/usr/local/bin/node22";
    let node_ver = |candidate: &str| {
        if candidate == node22 {
            Some("v22.14.0".to_string())
        } else {
            Some("v26.0.0".to_string())
        }
    };
    let which = |cmd: &str| {
        if cmd == "node22" {
            Some(PathBuf::from(node22))
        } else {
            None
        }
    };

    let runtime = resolve_codegraph_node_runtime(&ResolveCodegraphNodeSupportOptions {
        node_version: Some(&node_ver),
        which: Some(&which),
        ..Default::default()
    });
    let support = resolve_codegraph_node_support(&ResolveCodegraphNodeSupportOptions {
        node_version: Some(&node_ver),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(runtime, Some("/usr/local/bin/node22".to_string()));
    assert_eq!(
        support,
        CodegraphNodeSupport {
            major: 22,
            r#override: false,
            reason: None,
            supported: true,
        }
    );
}

#[test]
fn resolve_node_runtime_selects_homebrew_keg() {
    let node22 = "/opt/homebrew/opt/node@22/bin/node";
    let file_exists = move |p: &Path| p == Path::new(node22);
    let node_ver = |candidate: &str| {
        if candidate == node22 {
            Some("v22.23.0".to_string())
        } else {
            Some("v26.3.1".to_string())
        }
    };
    let which = |_cmd: &str| None;

    let runtime = resolve_codegraph_node_runtime(&ResolveCodegraphNodeSupportOptions {
        file_exists: Some(&file_exists),
        node_version: Some(&node_ver),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(runtime, Some(node22.to_string()));
}

#[test]
fn resolve_node_support_unsupported_node26() {
    let node = "/usr/local/bin/node";
    let node_ver = |candidate: &str| {
        if candidate == node {
            Some("v26.0.0".to_string())
        } else {
            Some("v0.0.0".to_string())
        }
    };
    let which = |cmd: &str| {
        if cmd == "node" {
            Some(PathBuf::from(node))
        } else {
            None
        }
    };

    let support = resolve_codegraph_node_support(&ResolveCodegraphNodeSupportOptions {
        node_version: Some(&node_ver),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        support,
        CodegraphNodeSupport {
            major: 0,
            r#override: false,
            reason: Some(CodegraphNodeUnsupportedReason::TooOld),
            supported: false,
        }
    );
}

#[test]
fn resolve_node_runtime_configured_node_bin() {
    let mut env = BTreeMap::new();
    env.insert("CODEGRAPH_NODE_BIN".to_string(), "node22".to_string());
    let node22 = "/opt/homebrew/bin/node22";

    let node_ver = |candidate: &str| {
        if candidate == node22 {
            Some("22.17.1".to_string())
        } else {
            Some("26.0.0".to_string())
        }
    };
    let which = |cmd: &str| {
        if cmd == "node22" {
            Some(PathBuf::from(node22))
        } else {
            None
        }
    };

    let runtime = resolve_codegraph_node_runtime(&ResolveCodegraphNodeSupportOptions {
        env: Some(env),
        node_version: Some(&node_ver),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(runtime, Some(node22.to_string()));
}

#[test]
fn resolve_command_uses_provisioned_before_path() {
    let provisioned = "/home/me/.omo/codegraph/bin/codegraph";
    let file_exists = |_p: &Path| true;
    let prov = move || Some(PathBuf::from(provisioned));
    let req_resolve = |_spec: &str| None;
    let which = |_cmd: &str| Some(PathBuf::from("/usr/local/bin/codegraph"));

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        file_exists: Some(&file_exists),
        provisioned: Some(&prov),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: provisioned.to_string(),
            exists: true,
            source: CodegraphCommandSource::Provisioned,
        }
    );
}

#[test]
fn resolve_command_returns_path_tier_when_missing() {
    let file_exists = |_p: &Path| false;
    let prov = || None;
    let req_resolve = |_spec: &str| None;
    let which = |_cmd: &str| None;

    let result = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        file_exists: Some(&file_exists),
        home_dir: Some(PathBuf::from("/tmp/omo-codegraph-resolve-missing-home")),
        provisioned: Some(&prov),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        result,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: "codegraph".to_string(),
            exists: false,
            source: CodegraphCommandSource::Path,
        }
    );
}

#[test]
fn resolve_command_managed_runtime_stale_ignored() {
    let home = tempdir().unwrap();
    let home_path = home.path();
    let install_dir = home_path.join(".maho").join("codegraph");
    let bin_name = if cfg!(windows) {
        "codegraph.cmd"
    } else {
        "codegraph"
    };
    let stale_bin = install_dir.join("bin").join(bin_name);
    let stale_marker = install_dir
        .join(".provisioned")
        .join("codegraph-1.0.1.json");

    fs::create_dir_all(install_dir.join("bin")).unwrap();
    fs::create_dir_all(install_dir.join(".provisioned")).unwrap();
    fs::write(&stale_bin, "").unwrap();
    fs::write(
        &stale_marker,
        format!(
            "{}\n",
            serde_json::json!({ "binPath": stale_bin.to_string_lossy(), "version": "1.0.1" })
        ),
    )
    .unwrap();

    let node_rt = || None;
    let req_resolve = |_spec: &str| None;
    let which = |_cmd: &str| None;

    let resolution = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        env: Some(BTreeMap::new()),
        home_dir: Some(home_path.to_path_buf()),
        node_runtime: Some(&node_rt),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        resolution,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: "codegraph".to_string(),
            exists: false,
            source: CodegraphCommandSource::Path,
        }
    );
}

#[test]
fn resolve_command_managed_runtime_pinned_accepted() {
    let home = tempdir().unwrap();
    let home_path = home.path();
    let install_dir = home_path.join(".maho").join("codegraph");
    let bin_name = if cfg!(windows) {
        "codegraph.cmd"
    } else {
        "codegraph"
    };
    let pinned_bin = install_dir.join("bin").join(bin_name);
    let pinned_marker = install_dir
        .join(".provisioned")
        .join(format!("codegraph-{CODEGRAPH_PINNED_VERSION}.json"));

    fs::create_dir_all(install_dir.join("bin")).unwrap();
    fs::create_dir_all(install_dir.join(".provisioned")).unwrap();
    fs::write(&pinned_bin, "").unwrap();
    fs::write(
        &pinned_marker,
        format!(
            "{}\n",
            serde_json::json!({ "binPath": pinned_bin.to_string_lossy(), "version": CODEGRAPH_PINNED_VERSION })
        ),
    )
    .unwrap();

    let node_rt = || None;
    let req_resolve = |_spec: &str| None;
    let which = |_cmd: &str| None;

    let resolution = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        env: Some(BTreeMap::new()),
        home_dir: Some(home_path.to_path_buf()),
        node_runtime: Some(&node_rt),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        resolution,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: pinned_bin.to_string_lossy().into_owned(),
            exists: true,
            source: CodegraphCommandSource::Provisioned,
        }
    );
}

#[test]
fn resolve_command_managed_runtime_external_rejected() {
    let home = tempdir().unwrap();
    let home_path = home.path();
    let install_dir = home_path.join(".maho").join("codegraph");
    let bin_name = if cfg!(windows) {
        "codegraph.cmd"
    } else {
        "codegraph"
    };
    let expected_bin = install_dir.join("bin").join(bin_name);
    let external_bin = home_path.join("external-codegraph");
    let pinned_marker = install_dir
        .join(".provisioned")
        .join(format!("codegraph-{CODEGRAPH_PINNED_VERSION}.json"));

    fs::create_dir_all(install_dir.join("bin")).unwrap();
    fs::create_dir_all(install_dir.join(".provisioned")).unwrap();
    fs::write(&expected_bin, "").unwrap();
    fs::write(&external_bin, "").unwrap();
    fs::write(
        &pinned_marker,
        format!(
            "{}\n",
            serde_json::json!({ "binPath": external_bin.to_string_lossy(), "version": CODEGRAPH_PINNED_VERSION })
        ),
    )
    .unwrap();

    let node_rt = || None;
    let req_resolve = |_spec: &str| None;
    let which = |_cmd: &str| None;

    let resolution = resolve_codegraph_command(&ResolveCodegraphCommandOptions {
        env: Some(BTreeMap::new()),
        home_dir: Some(home_path.to_path_buf()),
        node_runtime: Some(&node_rt),
        require_resolve: Some(&req_resolve),
        which: Some(&which),
        ..Default::default()
    });

    assert_eq!(
        resolution,
        CodegraphCommandResolution {
            args_prefix: vec![],
            command: "codegraph".to_string(),
            exists: false,
            source: CodegraphCommandSource::Path,
        }
    );
}

#[test]
fn guidance_uninitialized_project_extracted() {
    let output = "Tool execution failed: CodeGraph not initialized in /Users/me/project. Run 'codegraph init' in that project first.";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: None,
        tool_name: Some("codegraph.codegraph_status".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, Some("/Users/me/project".to_string()));
}

#[test]
fn guidance_non_codegraph_tool_ignored() {
    let output = "Tool execution failed: CodeGraph not initialized in /Users/me/project. Run 'codegraph init' in that project first.";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: None,
        tool_name: Some("bash".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, None);
}

#[test]
fn guidance_status_output_extracted() {
    let output =
        "Project: /Users/me/project\nNot initialized\nRun \"codegraph init\" to initialize";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: None,
        tool_name: Some("mcp__codegraph__codegraph_status".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, Some("/Users/me/project".to_string()));
}

#[test]
fn guidance_decorated_status_output_extracted() {
    let output = "\u{1b}[36m* Project:\u{1b}[0m /Users/me/project\n\u{1b}[31m! Not initialized\u{1b}[0m\n\u{1b}[2mRun \"codegraph init\" to initialize\u{1b}[0m";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: None,
        tool_name: Some("mcp__codegraph__codegraph_status".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, Some("/Users/me/project".to_string()));
}

#[test]
fn guidance_cwd_fallback() {
    let output = "Not initialized\nRun \"codegraph init\" to initialize";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: Some("/Users/me/project".to_string()),
        tool_name: Some("codegraph.codegraph_status".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, Some("/Users/me/project".to_string()));
}

#[test]
fn guidance_not_indexed_extracted() {
    let output = "The project at /Users/me/project isn't indexed with codegraph (no .codegraph/ directory found walking up from it), so codegraph cannot query it. Use your built-in tools (Read/Grep/Glob) for that codebase instead, and don't call codegraph for it again this session. Indexing is the user's decision — they can run 'codegraph init' in that project to enable it.";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: None,
        tool_name: Some("codegraph_explore".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, Some("/Users/me/project".to_string()));
}

#[test]
fn guidance_no_project_loaded_extracted() {
    let output = "No CodeGraph project is loaded for this session.\nSearched for a .codegraph/ directory starting from: /Users/me/project\nEither the server root has no index of its own";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: None,
        tool_name: Some("codegraph_explore".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, Some("/Users/me/project".to_string()));
}

#[test]
fn guidance_not_indexed_non_codegraph_tool_ignored() {
    let output = "The project at /Users/me/project isn't indexed with codegraph (no .codegraph/ directory found walking up from it), so codegraph cannot query it.";
    let project_path = get_codegraph_uninitialized_project(&CodegraphInitGuidanceInput {
        cwd: None,
        tool_name: Some("bash".to_string()),
        tool_output: output.to_string(),
    });
    assert_eq!(project_path, None);
}

#[test]
fn guidance_built_points_to_omo_global_store() {
    let guidance = build_codegraph_init_guidance(
        "/Users/me/project",
        &CodegraphInitGuidanceOptions {
            home_dir: Some(PathBuf::from("/Users/me")),
        },
    );
    let normalized = guidance.replace(r"\\", "/").replace('\\', "/");
    assert!(normalized.contains("CodeGraph is not initialized for \"/Users/me/project\""));
    assert!(normalized.contains("/Users/me/project/.codegraph\""));
    assert!(normalized.contains("\"/Users/me/.maho/codegraph/projects/project-"));
    assert!(normalized.contains("run `codegraph init` from \"/Users/me/project\""));
    assert!(!normalized.contains("Run 'codegraph init' in that project first."));
}

#[test]
fn guidance_escapes_markdown_control_chars() {
    let guidance = build_codegraph_init_guidance(
        "/Users/me/project`\nINJECT",
        &CodegraphInitGuidanceOptions {
            home_dir: Some(PathBuf::from("/Users/me")),
        },
    );
    assert!(guidance.contains(r#""/Users/me/project`\nINJECT""#));
    assert!(!guidance.contains("project`\nINJECT"));
}
