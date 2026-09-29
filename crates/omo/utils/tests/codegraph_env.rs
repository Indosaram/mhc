use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use pretty_assertions::assert_eq;
use tempfile::tempdir;
use utils::codegraph::*;

#[test]
fn build_codegraph_env_defaults_to_daemon_on() {
    let home_dir = PathBuf::from("/Users/alice");
    let result = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(home_dir.clone()),
        daemon: None,
    });

    let mut expected_map = BTreeMap::new();
    expected_map.insert(
        CODEGRAPH_INSTALL_DIR_ENV.to_string(),
        home_dir
            .join(".maho")
            .join("codegraph")
            .to_string_lossy()
            .into_owned(),
    );
    expected_map.insert(CODEGRAPH_NO_DOWNLOAD_ENV.to_string(), "1".to_string());
    expected_map.insert(CODEGRAPH_TELEMETRY_ENV.to_string(), "0".to_string());
    expected_map.insert(DO_NOT_TRACK_ENV.to_string(), "1".to_string());

    assert_eq!(result.to_map(), expected_map);
    assert_eq!(result.daemon, None);
}

#[test]
fn build_codegraph_env_omits_daemon_when_enabled() {
    let home_dir = PathBuf::from("/Users/alice");
    let result = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(home_dir.clone()),
        daemon: Some(true),
    });

    let mut expected_map = BTreeMap::new();
    expected_map.insert(
        CODEGRAPH_INSTALL_DIR_ENV.to_string(),
        home_dir
            .join(".maho")
            .join("codegraph")
            .to_string_lossy()
            .into_owned(),
    );
    expected_map.insert(CODEGRAPH_NO_DOWNLOAD_ENV.to_string(), "1".to_string());
    expected_map.insert(CODEGRAPH_TELEMETRY_ENV.to_string(), "0".to_string());
    expected_map.insert(DO_NOT_TRACK_ENV.to_string(), "1".to_string());

    assert_eq!(result.to_map(), expected_map);
    assert_eq!(result.daemon, Some(true));
}

#[test]
fn build_codegraph_child_env_filters_secrets() {
    let mut ambient_env = BTreeMap::new();
    ambient_env.insert(
        "ANTHROPIC_API_KEY".to_string(),
        "anthropic-secret".to_string(),
    );
    ambient_env.insert("GITHUB_TOKEN".to_string(), "github-secret".to_string());
    ambient_env.insert("HOME".to_string(), "/Users/alice".to_string());
    ambient_env.insert("OPENAI_API_KEY".to_string(), "openai-secret".to_string());
    ambient_env.insert("PATH".to_string(), "/usr/local/bin:/usr/bin".to_string());

    let mut codegraph_env = BTreeMap::new();
    codegraph_env.insert(
        "CODEGRAPH_INSTALL_DIR".to_string(),
        "/Users/alice/.omo/codegraph".to_string(),
    );
    codegraph_env.insert("CODEGRAPH_NO_DOWNLOAD".to_string(), "1".to_string());

    let mut runtime_env = BTreeMap::new();
    runtime_env.insert(
        "CODEGRAPH_FAKE_LOG".to_string(),
        "/tmp/codegraph.log".to_string(),
    );
    runtime_env.insert("SLACK_BOT_TOKEN".to_string(), "slack-secret".to_string());

    let result = build_codegraph_child_env(&BuildCodegraphChildEnvOptions {
        ambient_env: Some(ambient_env),
        codegraph_env: Some(codegraph_env),
        runtime_env: Some(runtime_env),
    });

    let mut expected = BTreeMap::new();
    expected.insert(
        "CODEGRAPH_FAKE_LOG".to_string(),
        "/tmp/codegraph.log".to_string(),
    );
    expected.insert(
        "CODEGRAPH_INSTALL_DIR".to_string(),
        "/Users/alice/.omo/codegraph".to_string(),
    );
    expected.insert("CODEGRAPH_NO_DOWNLOAD".to_string(), "1".to_string());
    expected.insert("HOME".to_string(), "/Users/alice".to_string());
    expected.insert("PATH".to_string(), "/usr/local/bin:/usr/bin".to_string());

    assert_eq!(result, expected);
}

#[test]
fn codegraph_no_daemon_precedence_defaults_to_unset() {
    let codegraph_env = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(PathBuf::from("/Users/alice")),
        daemon: None,
    });

    let result = build_codegraph_child_env(&BuildCodegraphChildEnvOptions {
        ambient_env: None,
        codegraph_env: Some(codegraph_env.to_map()),
        runtime_env: Some(BTreeMap::new()),
    });

    assert_eq!(result.contains_key(CODEGRAPH_NO_DAEMON_ENV), false);
}

#[test]
fn codegraph_no_daemon_precedence_keeps_daemon_off() {
    let codegraph_env = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(PathBuf::from("/Users/alice")),
        daemon: Some(false),
    });

    let mut runtime_env = BTreeMap::new();
    runtime_env.insert(CODEGRAPH_NO_DAEMON_ENV.to_string(), "0".to_string());

    let result = build_codegraph_child_env(&BuildCodegraphChildEnvOptions {
        ambient_env: None,
        codegraph_env: Some(codegraph_env.to_map()),
        runtime_env: Some(runtime_env),
    });

    assert_eq!(result.get(CODEGRAPH_NO_DAEMON_ENV), Some(&"1".to_string()));
}

#[test]
fn codegraph_no_daemon_precedence_omits_when_enabled() {
    let codegraph_env = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(PathBuf::from("/Users/alice")),
        daemon: Some(true),
    });

    let result = build_codegraph_child_env(&BuildCodegraphChildEnvOptions {
        ambient_env: None,
        codegraph_env: Some(codegraph_env.to_map()),
        runtime_env: Some(BTreeMap::new()),
    });

    assert_eq!(result.contains_key(CODEGRAPH_NO_DAEMON_ENV), false);
}

#[test]
fn codegraph_no_daemon_precedence_honors_ambient_escape_hatch() {
    let codegraph_env = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(PathBuf::from("/Users/alice")),
        daemon: Some(true),
    });

    let mut runtime_env = BTreeMap::new();
    runtime_env.insert(CODEGRAPH_NO_DAEMON_ENV.to_string(), "1".to_string());

    let result = build_codegraph_child_env(&BuildCodegraphChildEnvOptions {
        ambient_env: None,
        codegraph_env: Some(codegraph_env.to_map()),
        runtime_env: Some(runtime_env),
    });

    assert_eq!(result.get(CODEGRAPH_NO_DAEMON_ENV), Some(&"1".to_string()));
}

#[test]
fn codegraph_child_env_forwards_daemon_idle_timeout() {
    let codegraph_env = build_codegraph_env(&BuildCodegraphEnvOptions {
        home_dir: Some(PathBuf::from("/Users/alice")),
        daemon: Some(true),
    });

    let mut runtime_env = BTreeMap::new();
    runtime_env.insert(
        "CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS".to_string(),
        "10000".to_string(),
    );

    let result = build_codegraph_child_env(&BuildCodegraphChildEnvOptions {
        ambient_env: None,
        codegraph_env: Some(codegraph_env.to_map()),
        runtime_env: Some(runtime_env),
    });

    assert_eq!(
        result.get("CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS"),
        Some(&"10000".to_string())
    );
}

#[test]
fn should_exclude_codegraph_project_default_and_omo_state() {
    #[cfg(unix)]
    {
        let tmp_path = tempfile::Builder::new()
            .prefix("omo-codegraph-excluded-")
            .tempdir_in("/tmp")
            .unwrap();
        let decision = should_exclude_codegraph_project(
            tmp_path.path(),
            &CodegraphProjectExclusionOptions {
                excluded_roots: None,
                home_dir: None,
                platform: Some("linux".to_string()),
                tmpdir: Some(PathBuf::from("/tmp/omo-current-temp")),
            },
        );
        assert_eq!(
            decision,
            CodegraphProjectExclusionDecision {
                excluded: true,
                matched_root: Some("/tmp".to_string()),
                reason: Some(CodegraphProjectExclusionReason::TmpRoot),
            }
        );
    }

    let darwin_tmp = should_exclude_codegraph_project(
        Path::new("/var/folders/ab/xyz/T/repo"),
        &CodegraphProjectExclusionOptions {
            excluded_roots: None,
            home_dir: None,
            platform: Some("darwin".to_string()),
            tmpdir: Some(PathBuf::from("/var/folders/ab/xyz/T")),
        },
    );
    assert_eq!(
        darwin_tmp,
        CodegraphProjectExclusionDecision {
            excluded: true,
            matched_root: Some("/var/folders/ab/xyz/T".to_string()),
            reason: Some(CodegraphProjectExclusionReason::TmpRoot),
        }
    );

    let win_tmp = should_exclude_codegraph_project(
        Path::new("C:\\Users\\x\\AppData\\Local\\Temp\\Repo"),
        &CodegraphProjectExclusionOptions {
            excluded_roots: None,
            home_dir: None,
            platform: Some("win32".to_string()),
            tmpdir: Some(PathBuf::from("C:\\Users\\x\\AppData\\Local\\Temp")),
        },
    );
    assert_eq!(
        win_tmp,
        CodegraphProjectExclusionDecision {
            excluded: true,
            matched_root: Some("C:\\Users\\x\\AppData\\Local\\Temp".to_string()),
            reason: Some(CodegraphProjectExclusionReason::TmpRoot),
        }
    );

    let allowed = should_exclude_codegraph_project(
        Path::new("/Users/alice/repo"),
        &CodegraphProjectExclusionOptions {
            excluded_roots: None,
            home_dir: None,
            platform: Some("darwin".to_string()),
            tmpdir: Some(PathBuf::from("/var/folders/ab/xyz/T")),
        },
    );
    assert_eq!(
        allowed,
        CodegraphProjectExclusionDecision {
            excluded: false,
            matched_root: None,
            reason: None,
        }
    );

    let omo_state = should_exclude_codegraph_project(
        Path::new("/Users/alice/repo/.omo/ulw-research/run/clones/repo"),
        &CodegraphProjectExclusionOptions::default(),
    );
    assert_eq!(
        omo_state,
        CodegraphProjectExclusionDecision {
            excluded: true,
            matched_root: Some(".omo".to_string()),
            reason: Some(CodegraphProjectExclusionReason::OmoState),
        }
    );
}

#[test]
fn should_exclude_codegraph_project_custom_roots() {
    let tmp = tempdir().unwrap();
    let home = tmp.path();
    let excluded_root = home.join("research-cache");
    let excluded_workspace = excluded_root.join("repo");
    let allowed_workspace = home.join("research-cache-sibling").join("repo");

    let options = CodegraphProjectExclusionOptions {
        excluded_roots: Some(vec!["~/research-cache".to_string()]),
        home_dir: Some(home.to_path_buf()),
        platform: Some("win32".to_string()),
        tmpdir: Some(PathBuf::from("C:\\Users\\x\\AppData\\Local\\Temp")),
    };

    assert_eq!(
        should_exclude_codegraph_project(&excluded_workspace, &options),
        CodegraphProjectExclusionDecision {
            excluded: true,
            matched_root: Some("~/research-cache".to_string()),
            reason: Some(CodegraphProjectExclusionReason::CustomRoot),
        }
    );
    assert_eq!(
        should_exclude_codegraph_project(&allowed_workspace, &options),
        CodegraphProjectExclusionDecision {
            excluded: false,
            matched_root: None,
            reason: None,
        }
    );
}

#[test]
fn should_exclude_codegraph_project_relative_custom_roots() {
    let tmp = tempdir().unwrap();
    let home = tmp.path();
    let excluded_workspace = home.join("research-cache").join("repo");
    let allowed_workspace = home.join("other-cache").join("repo");

    let options = CodegraphProjectExclusionOptions {
        excluded_roots: Some(vec!["research-cache".to_string()]),
        home_dir: Some(home.to_path_buf()),
        platform: Some("win32".to_string()),
        tmpdir: Some(PathBuf::from("C:\\Users\\x\\AppData\\Local\\Temp")),
    };

    assert_eq!(
        should_exclude_codegraph_project(&excluded_workspace, &options),
        CodegraphProjectExclusionDecision {
            excluded: true,
            matched_root: Some("research-cache".to_string()),
            reason: Some(CodegraphProjectExclusionReason::CustomRoot),
        }
    );
    assert_eq!(
        should_exclude_codegraph_project(&allowed_workspace, &options),
        CodegraphProjectExclusionDecision {
            excluded: false,
            matched_root: None,
            reason: None,
        }
    );
}

#[test]
fn evaluate_codegraph_node_support_matrix() {
    let supported = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: Some(BTreeMap::new()),
        node_version: Some("22.14.0".to_string()),
    });
    assert_eq!(
        supported,
        CodegraphNodeSupport {
            major: 22,
            r#override: false,
            reason: None,
            supported: true,
        }
    );

    let too_new = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: Some(BTreeMap::new()),
        node_version: Some("v26.3.0".to_string()),
    });
    assert_eq!(
        too_new,
        CodegraphNodeSupport {
            major: 26,
            r#override: false,
            reason: Some(CodegraphNodeUnsupportedReason::TooNew),
            supported: false,
        }
    );

    let too_old = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: Some(BTreeMap::new()),
        node_version: Some("18.20.4".to_string()),
    });
    assert_eq!(
        too_old,
        CodegraphNodeSupport {
            major: 18,
            r#override: false,
            reason: Some(CodegraphNodeUnsupportedReason::TooOld),
            supported: false,
        }
    );

    let mut env = BTreeMap::new();
    env.insert(CODEGRAPH_UNSAFE_NODE_ENV.to_string(), "1".to_string());
    let forced = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: Some(env),
        node_version: Some("26.3.0".to_string()),
    });
    assert_eq!(
        forced,
        CodegraphNodeSupport {
            major: 26,
            r#override: true,
            reason: Some(CodegraphNodeUnsupportedReason::TooNew),
            supported: true,
        }
    );

    let unparseable = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: Some(BTreeMap::new()),
        node_version: Some("not-a-version".to_string()),
    });
    assert_eq!(
        unparseable,
        CodegraphNodeSupport {
            major: 0,
            r#override: false,
            reason: Some(CodegraphNodeUnsupportedReason::TooOld),
            supported: false,
        }
    );
}

#[test]
fn build_codegraph_node_skip_hint_formatting() {
    let support_new = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: Some(BTreeMap::new()),
        node_version: Some("26.3.0".to_string()),
    });
    let hint_new = build_codegraph_node_skip_hint(&support_new);
    assert!(hint_new.ends_with('\n'));
    assert!(!hint_new.trim_end().contains('\n'));
    assert!(hint_new.contains("CodeGraph MCP skipped"));
    assert!(hint_new.contains(CODEGRAPH_UNSAFE_NODE_ENV));
    assert!(hint_new.contains(&CODEGRAPH_MIN_NODE_MAJOR.to_string()));
    assert!(hint_new.contains(&(CODEGRAPH_BLOCKED_NODE_MAJOR - 1).to_string()));

    let support_old = evaluate_codegraph_node_support(&EvaluateCodegraphNodeSupportOptions {
        env: Some(BTreeMap::new()),
        node_version: Some("18.0.0".to_string()),
    });
    let hint_old = build_codegraph_node_skip_hint(&support_old);
    assert!(hint_old.contains("too old"));
    assert!(hint_old.contains(&CODEGRAPH_MIN_NODE_MAJOR.to_string()));
}

#[test]
fn codegraph_provision_manifest_contract() {
    let manifest = codegraph_provision_manifest();
    assert_eq!(manifest.version, CODEGRAPH_PINNED_VERSION);
    assert_eq!(CODEGRAPH_PINNED_VERSION, "1.5.0");

    let mut platforms: Vec<_> = manifest.assets.keys().cloned().collect();
    platforms.sort();
    let mut expected_platforms = vec![
        "darwin-arm64".to_string(),
        "darwin-x64".to_string(),
        "linux-arm64".to_string(),
        "linux-x64".to_string(),
        "win32-arm64".to_string(),
        "win32-x64".to_string(),
    ];
    expected_platforms.sort();
    assert_eq!(platforms, expected_platforms);

    for (platform, asset) in &manifest.assets {
        assert!(asset.url.contains("1.5.0"));
        assert_eq!(
            asset.sha256.len(),
            64,
            "sha256 for {platform} should be 64 hex chars"
        );
        assert!(
            asset.sha256.chars().all(|c| c.is_ascii_hexdigit()),
            "sha256 for {platform} should be hex"
        );
    }

    let win_arm64 = manifest.assets.get("win32-arm64").unwrap();
    let win_x64 = manifest.assets.get("win32-x64").unwrap();
    assert_eq!(win_arm64.executable_name, "codegraph.cmd");
    assert_eq!(win_x64.executable_name, "codegraph.cmd");
    assert_eq!(
        win_arm64.url,
        "https://registry.npmjs.org/@colbymchenry/codegraph-win32-arm64/-/codegraph-win32-arm64-1.5.0.tgz"
    );
    assert_eq!(
        win_x64.url,
        "https://registry.npmjs.org/@colbymchenry/codegraph-win32-x64/-/codegraph-win32-x64-1.5.0.tgz"
    );

    let darwin_arm64 = manifest.assets.get("darwin-arm64").unwrap();
    let darwin_x64 = manifest.assets.get("darwin-x64").unwrap();
    let linux_arm64 = manifest.assets.get("linux-arm64").unwrap();
    let linux_x64 = manifest.assets.get("linux-x64").unwrap();
    assert_eq!(
        darwin_arm64.url,
        "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-darwin-arm64.tar.gz"
    );
    assert_eq!(
        darwin_x64.url,
        "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-darwin-x64.tar.gz"
    );
    assert_eq!(
        linux_arm64.url,
        "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-linux-arm64.tar.gz"
    );
    assert_eq!(
        linux_x64.url,
        "https://github.com/colbymchenry/codegraph/releases/download/v1.5.0/codegraph-linux-x64.tar.gz"
    );
}
