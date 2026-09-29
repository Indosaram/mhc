use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use omo_config_core::{
    LoadOmoConfigOptions, MAX_PROJECT_CONFIG_DIRECTORY_DEPTH, OmoConfigReadFileSystem,
    ResolveOmoConfigPathsOptions, ResolveOmoConfigViewOptions,
    find_project_config_paths_farthest_first, is_omo_telemetry_enabled, load_omo_config,
    merge_omo_config_records, resolve_home_dir, resolve_omo_config_paths, resolve_omo_config_view,
    resolve_omo_profile_name, resolve_user_omo_config_directory, resolve_user_omo_config_path,
};
use serde_json::{Map, Value, json};

fn write_file(path: &str, content: &str) {
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent).expect("parent directory");
    }
    std::fs::write(path, content).expect("write fixture");
}

fn env_of(pairs: &[(&str, &str)]) -> omo_config_core::OmoConfigEnv {
    pairs
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

#[derive(Default, Clone)]
struct FakeReadFileSystem {
    files: BTreeMap<String, String>,
    directories: BTreeSet<String>,
    symlinks: BTreeSet<String>,
    lstat: bool,
    read_always: Option<String>,
    unreadable: BTreeSet<String>,
    checked: RefCell<Vec<String>>,
}

impl FakeReadFileSystem {
    fn with_file(mut self, path: &str) -> Self {
        self.files.insert(path.to_string(), "{}".to_string());
        self
    }

    fn read_result(&self, path: &str) -> Result<String, std::io::Error> {
        if self.unreadable.contains(path) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "EACCES synthetic",
            ));
        }
        if let Some(content) = &self.read_always {
            return Ok(content.clone());
        }
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, path.to_string()))
    }
}

impl OmoConfigReadFileSystem for FakeReadFileSystem {
    fn exists(&self, path: &str) -> bool {
        self.checked.borrow_mut().push(path.to_string());
        self.files.contains_key(path) || self.directories.contains(path)
    }

    fn read(&self, path: &str) -> Result<String, std::io::Error> {
        self.read_result(path)
    }

    fn is_symbolic_link(&self, path: &str) -> Option<bool> {
        if !self.lstat {
            return None;
        }
        Some(self.symlinks.contains(path))
    }

    fn realpath(&self, _path: &str) -> Option<String> {
        None
    }
}

#[test]
fn merge_removes_nested_unsafe_keys_when_assigning_new_objects() {
    let parsed = json!({
        "categories": {
            "quick": {
                "tools": {
                    "bash": true,
                    "__proto__": { "polluted": true },
                    "constructor": { "polluted": true },
                    "prototype": { "polluted": true }
                }
            }
        }
    });
    let base = Map::new();
    let merged = merge_omo_config_records(&base, parsed.as_object().expect("object"), None);
    let tools = merged["categories"]["quick"]["tools"]
        .as_object()
        .expect("tools")
        .clone();
    assert_eq!(tools.get("bash"), Some(&json!(true)));
    assert!(!tools.contains_key("__proto__"));
    assert!(!tools.contains_key("constructor"));
    assert!(!tools.contains_key("prototype"));
}

#[test]
fn resolve_home_dir_preserves_an_absolute_posix_home() {
    assert_eq!(
        resolve_home_dir(&env_of(&[("HOME", "/home/alice")])),
        "/home/alice"
    );
}

#[test]
fn resolve_user_omo_config_directory_is_dot_omo_under_home() {
    let env = env_of(&[("HOME", "/home/alice")]);
    assert_eq!(resolve_user_omo_config_directory(&env), "/home/alice/.maho");
}

#[test]
fn resolve_user_omo_config_directory_ignores_xdg_and_appdata() {
    let env = env_of(&[
        ("HOME", "/home/alice"),
        ("XDG_CONFIG_HOME", "/home/alice/xdg"),
        ("APPDATA", "/home/alice/AppData/Roaming"),
    ]);
    assert_eq!(resolve_user_omo_config_directory(&env), "/home/alice/.maho");
}

#[test]
fn resolve_user_omo_config_directory_falls_back_to_userprofile() {
    let env = env_of(&[("USERPROFILE", "/home/alice")]);
    assert_eq!(resolve_user_omo_config_directory(&env), "/home/alice/.maho");
}

#[test]
fn resolve_user_omo_config_path_is_dot_omo_omo_jsonc() {
    let env = env_of(&[("HOME", "/home/alice")]);
    assert_eq!(
        resolve_user_omo_config_path(&env),
        "/home/alice/.maho/omo.jsonc"
    );
}

#[test]
fn resolve_omo_config_paths_selects_the_user_jsonc_candidate() {
    let file_system = FakeReadFileSystem::default()
        .with_file("/home/alice/.maho/omo.jsonc")
        .with_file("/home/alice/work/.omo/omo.jsonc");
    let candidates = resolve_omo_config_paths(&ResolveOmoConfigPathsOptions {
        cwd: "/home/alice/work".to_string(),
        env: Some(env_of(&[("HOME", "/home/alice")])),
        file_system: Some(&file_system),
        platform: Some("linux".to_string()),
    });
    assert_eq!(candidates[0].path, "/home/alice/.maho/omo.jsonc");
    assert_eq!(candidates[0].scope, "user");
    assert_eq!(candidates[1].path, "/home/alice/work/.omo/omo.jsonc");
    assert_eq!(candidates[1].scope, "project");
}

#[test]
fn resolve_omo_config_paths_prefers_the_user_jsonc_over_json() {
    let file_system = FakeReadFileSystem::default()
        .with_file("/home/alice/.maho/omo.jsonc")
        .with_file("/home/alice/.maho/omo.json");
    let candidates = resolve_omo_config_paths(&ResolveOmoConfigPathsOptions {
        cwd: "/home/alice/work".to_string(),
        env: Some(env_of(&[("HOME", "/home/alice")])),
        file_system: Some(&file_system),
        platform: Some("linux".to_string()),
    });
    assert_eq!(candidates[0].path, "/home/alice/.maho/omo.jsonc");
}

#[test]
fn resolve_omo_config_paths_falls_back_to_the_user_json() {
    let file_system = FakeReadFileSystem::default().with_file("/home/alice/.maho/omo.json");
    let candidates = resolve_omo_config_paths(&ResolveOmoConfigPathsOptions {
        cwd: "/home/alice/work".to_string(),
        env: Some(env_of(&[("HOME", "/home/alice")])),
        file_system: Some(&file_system),
        platform: Some("linux".to_string()),
    });
    assert_eq!(candidates[0].path, "/home/alice/.maho/omo.json");
}

#[test]
fn resolve_omo_config_paths_never_selects_the_legacy_config_tree() {
    let file_system = FakeReadFileSystem::default().with_file("/home/alice/.config/omo/omo.jsonc");
    let candidates = resolve_omo_config_paths(&ResolveOmoConfigPathsOptions {
        cwd: "/home/alice/work".to_string(),
        env: Some(env_of(&[("HOME", "/home/alice")])),
        file_system: Some(&file_system),
        platform: Some("linux".to_string()),
    });
    assert!(
        !candidates
            .iter()
            .any(|candidate| candidate.path == "/home/alice/.config/omo/omo.jsonc")
    );
    assert_eq!(candidates[0].path, "/home/alice/.maho/omo.jsonc");
}

#[test]
fn resolve_omo_config_paths_keeps_the_user_candidate_first() {
    let file_system = FakeReadFileSystem::default()
        .with_file("/home/alice/.maho/omo.jsonc")
        .with_file("/home/alice/work/project/.omo/omo.jsonc");
    let candidates = resolve_omo_config_paths(&ResolveOmoConfigPathsOptions {
        cwd: "/home/alice/work/project".to_string(),
        env: Some(env_of(&[("HOME", "/home/alice")])),
        file_system: Some(&file_system),
        platform: Some("linux".to_string()),
    });
    assert_eq!(
        candidates
            .iter()
            .map(|candidate| (candidate.path.as_str(), candidate.scope))
            .collect::<Vec<_>>(),
        vec![
            ("/home/alice/.maho/omo.jsonc", "user"),
            ("/home/alice/work/project/.omo/omo.jsonc", "project"),
        ]
    );
}

fn project_file_system(
    config_directories: &[&str],
    symlinked_omo_directories: &[&str],
) -> FakeReadFileSystem {
    let mut file_system = FakeReadFileSystem {
        lstat: true,
        ..FakeReadFileSystem::default()
    };
    for directory in config_directories {
        file_system.directories.insert(format!("{directory}/.omo"));
        file_system
            .files
            .insert(format!("{directory}/.omo/omo.jsonc"), "{}".to_string());
    }
    for directory in symlinked_omo_directories {
        file_system.symlinks.insert((*directory).to_string());
    }
    file_system
}

fn project_config_path(directory: &str) -> String {
    format!("{directory}/.omo/omo.jsonc")
}

#[test]
fn project_paths_find_a_grandparent_config_outside_home() {
    let file_system = project_file_system(&["/opt/work"], &[]);
    let paths = find_project_config_paths_farthest_first(
        "/opt/work/project/sub",
        "/home/alice",
        &file_system,
        None,
    );
    assert_eq!(paths, vec![project_config_path("/opt/work")]);
}

#[test]
fn project_paths_are_farthest_first_for_nested_configs_inside_home() {
    let file_system = project_file_system(
        &[
            "/home/alice/work",
            "/home/alice/work/project",
            "/home/alice/work/project/src",
        ],
        &[],
    );
    let paths = find_project_config_paths_farthest_first(
        "/home/alice/work/project/src",
        "/home/alice",
        &file_system,
        None,
    );
    assert_eq!(
        paths,
        vec![
            project_config_path("/home/alice/work"),
            project_config_path("/home/alice/work/project"),
            project_config_path("/home/alice/work/project/src"),
        ]
    );
}

#[test]
fn project_paths_exclude_home_itself() {
    let file_system = project_file_system(&["/home/alice", "/home/alice/work/project"], &[]);
    let paths = find_project_config_paths_farthest_first(
        "/home/alice/work/project/src",
        "/home/alice",
        &file_system,
        None,
    );
    assert_eq!(paths, vec![project_config_path("/home/alice/work/project")]);
}

#[test]
fn project_paths_exclude_the_account_home_when_home_is_overridden() {
    let file_system = project_file_system(&["/home/alice", "/home/alice/work/project"], &[]);
    let paths = find_project_config_paths_farthest_first(
        "/home/alice/work/project/src",
        "/tmp/isolated-home",
        &file_system,
        Some("/home/alice"),
    );
    assert_eq!(paths, vec![project_config_path("/home/alice/work/project")]);
}

#[test]
fn project_paths_refuse_a_symlinked_omo_directory() {
    let file_system = project_file_system(&["/opt/work/project"], &["/opt/work/project/.omo"]);
    let paths = find_project_config_paths_farthest_first(
        "/opt/work/project",
        "/home/alice",
        &file_system,
        None,
    );
    assert!(paths.is_empty());
}

#[test]
fn project_paths_are_empty_without_project_configs() {
    let file_system = project_file_system(&[], &[]);
    let paths = find_project_config_paths_farthest_first(
        "/opt/work/project",
        "/home/alice",
        &file_system,
        None,
    );
    assert!(paths.is_empty());
}

#[test]
fn project_paths_bound_the_walk_to_the_maximum_depth() {
    let file_system = FakeReadFileSystem {
        lstat: true,
        ..FakeReadFileSystem::default()
    };
    let mut cwd = String::from("/opt");
    for index in 0..(MAX_PROJECT_CONFIG_DIRECTORY_DEPTH + 1) {
        cwd = format!("{cwd}/level-{index}");
    }
    let paths = find_project_config_paths_farthest_first(&cwd, "/home/alice", &file_system, None);
    assert!(paths.is_empty());
    let checked = file_system.checked.borrow().clone();
    let checked_config_paths: Vec<&String> = checked
        .iter()
        .filter(|path| path.ends_with("/.omo/omo.jsonc"))
        .collect();
    assert_eq!(
        checked_config_paths.len(),
        MAX_PROJECT_CONFIG_DIRECTORY_DEPTH
    );
    assert!(!checked.iter().any(|path| path == "/opt/.omo/omo.jsonc"));
}

#[test]
fn resolve_omo_profile_name_prefers_the_explicit_option() {
    let env = env_of(&[
        ("OCX_PROFILE", "ocx"),
        ("OMO_PROFILE", "omo"),
        ("OPENCODE_CONFIG_DIR", "/does-not-exist/profiles/directory"),
    ]);
    assert_eq!(
        resolve_omo_profile_name(Some(&env), Some(&"explicit".to_string())),
        Some("explicit".to_string())
    );
}

#[test]
fn resolve_omo_profile_name_prefers_omo_profile_over_lower_precedence_sources() {
    let env = env_of(&[
        ("OCX_PROFILE", "ocx"),
        ("OMO_PROFILE", "omo"),
        ("OPENCODE_CONFIG_DIR", "/does-not-exist/profiles/directory"),
    ]);
    assert_eq!(
        resolve_omo_profile_name(Some(&env), None),
        Some("omo".to_string())
    );
}

#[test]
fn resolve_omo_profile_name_prefers_ocx_profile_over_the_config_dir() {
    let env = env_of(&[
        ("OCX_PROFILE", "ocx"),
        ("OPENCODE_CONFIG_DIR", "/does-not-exist/profiles/directory"),
    ]);
    assert_eq!(
        resolve_omo_profile_name(Some(&env), None),
        Some("ocx".to_string())
    );
}

#[test]
fn resolve_omo_profile_name_derives_the_profile_from_a_profiles_tail() {
    let env = env_of(&[(
        "OPENCODE_CONFIG_DIR",
        "/this-path-does-not-exist/profiles/kimi",
    )]);
    assert_eq!(
        resolve_omo_profile_name(Some(&env), None),
        Some("kimi".to_string())
    );
}

#[test]
fn resolve_omo_profile_name_ignores_a_config_dir_without_a_profiles_tail() {
    let env = env_of(&[("OPENCODE_CONFIG_DIR", "/this-path-does-not-exist/opencode")]);
    assert_eq!(resolve_omo_profile_name(Some(&env), None), None);
}

#[test]
fn resolve_omo_profile_name_is_absent_without_any_source() {
    let env = env_of(&[]);
    assert_eq!(resolve_omo_profile_name(Some(&env), None), None);
}

fn config_map(value: Value) -> Map<String, Value> {
    value.as_object().cloned().expect("object")
}

#[test]
fn resolve_omo_config_view_applies_every_layer_and_strips_control_keys() {
    let config = config_map(json!({
        "settings": { "base": "base", "winner": "base" },
        "[senpi]": { "settings": { "harness": "harness", "winner": "harness" } },
        "profiles": {
            "opus": {
                "settings": { "profileBase": "profile-base", "winner": "profile-base" },
                "[senpi]": { "settings": { "profileHarness": "profile-harness", "winner": "profile-harness" } },
            }
        },
    }));
    let result = resolve_omo_config_view(ResolveOmoConfigViewOptions {
        config: &config,
        harness: Some(&"senpi".to_string()),
        profile: Some(&"opus".to_string()),
    });
    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.profile, Some("opus".to_string()));
    assert_eq!(
        Value::Object(result.config.clone()),
        json!({
            "settings": {
                "base": "base",
                "harness": "harness",
                "profileBase": "profile-base",
                "profileHarness": "profile-harness",
                "winner": "profile-harness",
            }
        })
    );
    assert!(!result.config.contains_key("[senpi]"));
    assert!(!result.config.contains_key("profiles"));
}

#[test]
fn resolve_omo_config_view_diagnoses_an_absent_profile_and_uses_the_base_view() {
    let config = config_map(json!({
        "settings": { "base": "base" },
        "[senpi]": { "settings": { "harness": "harness" } },
        "profiles": { "opus": { "settings": { "profile": "opus" } } },
    }));
    let result = resolve_omo_config_view(ResolveOmoConfigViewOptions {
        config: &config,
        harness: Some(&"senpi".to_string()),
        profile: Some(&"ghost".to_string()),
    });
    assert_eq!(result.profile, None);
    assert_eq!(
        Value::Object(result.config.clone()),
        json!({ "settings": { "base": "base", "harness": "harness" } })
    );
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, "profile");
    assert_eq!(result.diagnostics[0].path, "profiles.ghost");
}

#[test]
fn resolve_omo_config_view_unions_codegraph_excluded_roots() {
    let config = config_map(json!({
        "codegraph": { "excluded_roots": ["/tmp/omo-base", "/tmp/omo-shared"] },
        "[codex]": { "codegraph": { "excluded_roots": ["/tmp/omo-shared", "/tmp/omo-codex"] } },
    }));
    let result = resolve_omo_config_view(ResolveOmoConfigViewOptions {
        config: &config,
        harness: Some(&"codex".to_string()),
        profile: None,
    });
    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        Value::Object(result.config.clone()),
        json!({ "codegraph": { "excluded_roots": ["/tmp/omo-base", "/tmp/omo-shared", "/tmp/omo-codex"] } })
    );
}

fn empty_options() -> LoadOmoConfigOptions<'static> {
    LoadOmoConfigOptions::default()
}

#[test]
fn load_walks_user_and_project_configs_with_the_nearest_project_winning() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let work = format!("{home}/work");
    let project = format!("{work}/project");
    let cwd = format!("{project}/child");
    std::fs::create_dir_all(&cwd).expect("cwd");

    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{
            "categories": { "quick": { "model": "user-model", "tools": { "read": true } } },
            "agents": { "reviewer": { "model": "user-agent", "tools": { "bash": false } } },
            "task": { "default_concurrency": 2, "wait": { "default_ms": 11000 } },
            "teams": { "alpha": { "members": [{ "name": "one", "kind": "category", "category": "quick", "prompt": "go" }] } }
        }"#,
    );
    write_file(
        &format!("{work}/.omo/omo.jsonc"),
        r#"{
            "categories": { "quick": { "model": "far-model", "tools": { "bash": true } }, "deep": { "model": "deep-model" } },
            "agents": { "reviewer": { "temperature": 0.3 } },
            "task": { "default_concurrency": 4, "wait": { "max_ms": 90000 } }
        }"#,
    );
    write_file(
        &format!("{project}/.omo/omo.jsonc"),
        r#"{
            "categories": { "quick": { "model": "near-model" } },
            "agents": { "reviewer": { "model": "near-agent" } },
            "task": { "default_concurrency": 7 }
        }"#,
    );
    write_file(
        &format!("{project}/.omo/oh-my-openagent.jsonc"),
        r#"{"task":{"default_concurrency":99}}"#,
    );
    write_file(
        &format!("{project}/.omo/oh-my-opencode.jsonc"),
        r#"{"task":{"default_concurrency":98}}"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.config["task"]["default_concurrency"], json!(7));
    assert_eq!(result.config["task"]["wait"]["default_ms"], json!(11000));
    assert_eq!(result.config["task"]["wait"]["max_ms"], json!(90000));
    assert_eq!(
        result.config["categories"]["quick"]["model"],
        json!("near-model")
    );
    assert_eq!(
        result.config["categories"]["quick"]["tools"],
        json!({ "read": true, "bash": true })
    );
    assert_eq!(
        result.config["categories"]["deep"]["model"],
        json!("deep-model")
    );
    assert_eq!(
        result.config["agents"]["reviewer"]["model"],
        json!("near-agent")
    );
    assert_eq!(
        result.config["agents"]["reviewer"]["temperature"],
        json!(0.3)
    );
    assert_eq!(
        result.config["teams"]["alpha"]["members"][0]["name"],
        json!("one")
    );
}

#[test]
fn load_merges_partial_team_layers() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let project = format!("{home}/project");
    let cwd = format!("{project}/child");
    std::fs::create_dir_all(&cwd).expect("cwd");

    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{"teams":{"alpha":{"members":[{"name":"one","kind":"category","category":"quick","prompt":"go"}]}}}"#,
    );
    write_file(
        &format!("{project}/.omo/omo.jsonc"),
        r#"{"teams":{"alpha":{"description":"near layer description"}}}"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        result.config["teams"]["alpha"]["description"],
        json!("near layer description")
    );
    assert_eq!(
        result.config["teams"]["alpha"]["members"][0]["name"],
        json!("one")
    );
}

#[test]
fn load_ignores_a_project_omo_directory_that_is_a_symlink() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let project = format!("{home}/project");
    let cwd = format!("{project}/child");
    std::fs::create_dir_all(&cwd).expect("cwd");
    std::fs::create_dir_all(&home).expect("home");
    let outside = format!("{home}/outside-omo");
    write_file(
        &format!("{outside}/omo.jsonc"),
        r#"{"task":{"default_concurrency":9}}"#,
    );
    std::os::unix::fs::symlink(&outside, format!("{project}/.omo")).expect("symlink");

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.config["task"]["default_concurrency"], json!(5));
    assert!(
        !result
            .sources
            .iter()
            .any(|source| source.scope == "project" && source.loaded)
    );
}

#[test]
fn load_ignores_a_symlinked_project_config_file() {
    for extension in ["jsonc", "json"] {
        let root = tempfile::tempdir().expect("tempdir");
        let root_path = root.path().to_string_lossy().to_string();
        let home = format!("{root_path}/home");
        let project = format!("{home}/project");
        let cwd = format!("{project}/child");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&home).expect("home");
        let outside = format!("{home}/outside.{extension}");
        write_file(&outside, r#"{"task":{"default_concurrency":9}}"#);
        let project_config = format!("{project}/.omo/omo.{extension}");
        std::fs::create_dir_all(format!("{project}/.omo")).expect("omo dir");
        std::os::unix::fs::symlink(&outside, &project_config).expect("symlink");

        let result = load_omo_config(&LoadOmoConfigOptions {
            cwd: Some(cwd.clone()),
            env: Some(env_of(&[("HOME", &home)])),
            platform: Some("linux".to_string()),
            ..empty_options()
        });

        assert_eq!(result.diagnostics, vec![], "{extension}");
        assert_eq!(
            result.config["task"]["default_concurrency"],
            json!(5),
            "{extension}"
        );
        assert!(
            !result
                .sources
                .iter()
                .any(|source| source.scope == "project" && source.loaded),
            "{extension}"
        );
    }
}

#[test]
fn load_survives_malformed_and_unreadable_configs_with_typed_diagnostics() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let project = format!("{home}/project");
    let cwd = format!("{project}/child");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let unreadable_path = format!("{project}/.omo/omo.jsonc");
    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{"task":{"default_concurrency":"five"}}"#,
    );
    write_file(&unreadable_path, r#"{"task":{"default_concurrency":3}}"#);

    let mut file_system = FakeReadFileSystem {
        read_always: Some(r#"{"task":{"default_concurrency":"five"}}"#.to_string()),
        ..FakeReadFileSystem::default()
    };
    file_system.files.insert(
        format!("{home}/.maho/omo.jsonc"),
        r#"{"task":{"default_concurrency":"five"}}"#.to_string(),
    );
    file_system.files.insert(
        unreadable_path.clone(),
        r#"{"task":{"default_concurrency":3}}"#.to_string(),
    );
    file_system.unreadable.insert(unreadable_path.clone());
    file_system.files.insert("/".to_string(), "{}".to_string());

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        file_system: Some(&file_system),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.config["task"]["default_concurrency"], json!(5));
    let kinds: Vec<&str> = result
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.kind)
        .collect();
    assert!(kinds.contains(&"validation"));
    assert!(kinds.contains(&"read"));
    assert!(
        result
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.path == unreadable_path)
    );
}

#[test]
fn load_reads_the_user_json_and_prefers_jsonc_when_both_exist() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let cwd = format!("{home}/project");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let json_path = format!("{home}/.maho/omo.json");
    write_file(&json_path, r#"{"task":{"default_concurrency":9}}"#);

    let json_only = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd.clone()),
        env: Some(env_of(&[("HOME", &home)])),
        platform: Some("linux".to_string()),
        ..empty_options()
    });
    assert_eq!(json_only.diagnostics, vec![]);
    assert_eq!(json_only.config["task"]["default_concurrency"], json!(9));
    assert_eq!(json_only.sources[0].path, json_path);
    assert_eq!(json_only.sources[0].scope, "user");
    assert!(json_only.sources[0].exists && json_only.sources[0].loaded);

    let second_root = tempfile::tempdir().expect("tempdir");
    let second_root_path = second_root.path().to_string_lossy().to_string();
    let second_home = format!("{second_root_path}/home");
    let second_cwd = format!("{second_home}/project");
    std::fs::create_dir_all(&second_cwd).expect("cwd");
    let jsonc_path = format!("{second_home}/.maho/omo.jsonc");
    write_file(
        &format!("{second_home}/.maho/omo.json"),
        r#"{"task":{"default_concurrency":8}}"#,
    );
    write_file(&jsonc_path, r#"{"task":{"default_concurrency":4}}"#);

    let both = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(second_cwd),
        env: Some(env_of(&[("HOME", &second_home)])),
        platform: Some("linux".to_string()),
        ..empty_options()
    });
    assert_eq!(both.diagnostics, vec![]);
    assert_eq!(both.config["task"]["default_concurrency"], json!(4));
    assert_eq!(both.sources[0].path, jsonc_path);
}

#[test]
fn load_tolerates_a_schema_key_without_diagnostics() {
    let schema_url =
        "https://raw.githubusercontent.com/code-yeongyu/oh-my-openagent/dev/assets/omo.schema.json";
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let cwd = format!("{home}/project");
    std::fs::create_dir_all(&cwd).expect("cwd");
    let user_path = format!("{home}/.maho/omo.jsonc");
    write_file(
        &user_path,
        &format!(r#"{{"$schema":"{schema_url}","task":{{"default_concurrency":3}}}}"#),
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.sources[0].path, user_path);
    assert_eq!(result.config["task"]["default_concurrency"], json!(3));
    assert_eq!(result.config["$schema"], json!(schema_url));
}

#[test]
fn load_reads_ancestor_project_configs_when_cwd_is_outside_home() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let outside_project = format!("{root_path}/elsewhere/project");
    std::fs::create_dir_all(&outside_project).expect("cwd");
    std::fs::create_dir_all(&home).expect("home");
    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{"task":{"default_concurrency":2}}"#,
    );
    write_file(
        &format!("{outside_project}/.omo/omo.jsonc"),
        r#"{"task":{"default_concurrency":7}}"#,
    );
    write_file(
        &format!("{root_path}/elsewhere/.omo/omo.jsonc"),
        r#"{"task":{"default_concurrency":9}}"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(outside_project),
        env: Some(env_of(&[("HOME", &home)])),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.config["task"]["default_concurrency"], json!(7));
    assert!(
        result
            .sources
            .iter()
            .any(|source| source.path == format!("{root_path}/elsewhere/.omo/omo.jsonc"))
    );
}

#[test]
fn load_resolution_contributes_every_layer_in_precedence_order() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let project = format!("{home}/project");
    let cwd = format!("{project}/child");
    std::fs::create_dir_all(&cwd).expect("cwd");
    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{
            "codegraph": { "enabled": false, "daemon": true },
            "[senpi]": { "codegraph": { "auto_provision": false, "daemon": false } },
            "profiles": {
                "opus": {
                    "codegraph": { "daemon": true, "telemetry": true },
                    "[senpi]": { "codegraph": { "daemon": false, "excluded_roots": ["/tmp/opus"] } }
                }
            }
        }"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home), ("OMO_PROFILE", "opus")])),
        harness: Some("senpi".to_string()),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.profile, Some("opus".to_string()));
    let codegraph = &result.config["codegraph"];
    assert_eq!(codegraph["enabled"], json!(false));
    assert_eq!(codegraph["auto_provision"], json!(false));
    assert_eq!(codegraph["daemon"], json!(false));
    assert_eq!(codegraph["telemetry"], json!(true));
    assert_eq!(codegraph["excluded_roots"], json!(["/tmp/opus"]));
    assert!(result.config.get("profiles").is_none());
    assert!(result.config.get("[senpi]").is_none());
}

#[test]
fn load_keeps_raw_layer_provenance_for_user_and_project() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let project = format!("{home}/project");
    let cwd = format!("{project}/child");
    std::fs::create_dir_all(&cwd).expect("cwd");
    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{"codegraph":{"enabled":false},"profiles":{"opus":{"codegraph":{"telemetry":true}}}}"#,
    );
    write_file(
        &format!("{project}/.omo/omo.jsonc"),
        r#"{"[senpi]":{"codegraph":{"auto_provision":false}},"profiles":{"opus":{"[senpi]":{"codegraph":{"daemon":false}}}}}"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home), ("OMO_PROFILE", "opus")])),
        harness: Some("senpi".to_string()),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    let codegraph = &result.config["codegraph"];
    assert_eq!(codegraph["enabled"], json!(false));
    assert_eq!(codegraph["auto_provision"], json!(false));
    assert_eq!(codegraph["daemon"], json!(false));
    assert_eq!(codegraph["telemetry"], json!(true));
    let scopes: Vec<&str> = result
        .layers
        .iter()
        .map(|layer| layer.source.scope)
        .collect();
    assert_eq!(scopes, vec!["user", "project"]);
    let configs: Vec<Value> = result
        .layers
        .iter()
        .map(|layer| layer.config.clone())
        .collect();
    assert_eq!(
        configs,
        vec![
            json!({ "codegraph": { "enabled": false }, "profiles": { "opus": { "codegraph": { "telemetry": true } } } }),
            json!({
                "[senpi]": { "codegraph": { "auto_provision": false } },
                "profiles": { "opus": { "[senpi]": { "codegraph": { "daemon": false } } } }
            }),
        ]
    );
}

#[test]
fn load_codex_view_does_not_let_defaults_overwrite_base_telemetry() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let cwd = format!("{home}/project");
    std::fs::create_dir_all(&cwd).expect("cwd");
    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{"codegraph":{"telemetry":true},"[codex]":{"codegraph":{}}}"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        harness: Some("codex".to_string()),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.config["codegraph"]["telemetry"], json!(true));
}

#[test]
fn load_codex_view_unions_overlapping_excluded_roots() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let project = format!("{home}/project");
    let cwd = format!("{project}/child");
    std::fs::create_dir_all(&cwd).expect("cwd");
    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{"codegraph":{"excluded_roots":["/tmp/omo-a","/tmp/omo-b"]}}"#,
    );
    write_file(
        &format!("{project}/.omo/omo.jsonc"),
        r#"{"[codex]":{"codegraph":{"excluded_roots":["/tmp/omo-b","/tmp/omo-c"]}}}"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        harness: Some("codex".to_string()),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(
        result.config["codegraph"]["excluded_roots"],
        json!(["/tmp/omo-a", "/tmp/omo-b", "/tmp/omo-c"])
    );
}

#[test]
fn load_reports_an_unknown_activated_profile_and_skips_its_overlay() {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let cwd = format!("{home}/project");
    std::fs::create_dir_all(&cwd).expect("cwd");
    write_file(
        &format!("{home}/.maho/omo.jsonc"),
        r#"{"codegraph":{"enabled":false},"[senpi]":{"codegraph":{"daemon":false}},"profiles":{"opus":{"codegraph":{"telemetry":true}}}}"#,
    );

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home), ("OMO_PROFILE", "ghost")])),
        harness: Some("senpi".to_string()),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.profile, None);
    assert_eq!(result.config["codegraph"]["enabled"], json!(false));
    assert_eq!(result.config["codegraph"]["daemon"], json!(false));
    assert_eq!(result.config["codegraph"]["telemetry"], json!(false));
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, "profile");
    assert_eq!(result.diagnostics[0].path, "profiles.ghost");
}

fn telemetry_fixture(content: &str) -> (tempfile::TempDir, String, String) {
    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let cwd = format!("{home}/project");
    std::fs::create_dir_all(format!("{home}/.maho")).expect("omo dir");
    std::fs::create_dir_all(&cwd).expect("cwd");
    write_file(&format!("{home}/.maho/omo.json"), content);
    (root, home, cwd)
}

fn load_senpi(
    home: &str,
    cwd: &str,
    profile: Option<&str>,
) -> omo_config_core::LoadOmoConfigResult {
    load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd.to_string()),
        env: Some(env_of(&[("HOME", home)])),
        harness: Some("senpi".to_string()),
        platform: Some("linux".to_string()),
        profile: profile.map(str::to_string),
        ..empty_options()
    })
}

#[test]
fn telemetry_resolution_reads_a_disabled_base_setting() {
    let (_root, home, cwd) = telemetry_fixture(r#"{"telemetry":{"enabled":false}}"#);
    let result = load_senpi(&home, &cwd, None);
    assert_eq!(result.diagnostics, vec![]);
    assert!(!is_omo_telemetry_enabled(&Value::Object(result.config)));
}

#[test]
fn telemetry_resolution_lets_the_senpi_block_enable_telemetry() {
    let (_root, home, cwd) = telemetry_fixture(
        r#"{"telemetry":{"enabled":false},"[senpi]":{"telemetry":{"enabled":true}}}"#,
    );
    let result = load_senpi(&home, &cwd, None);
    assert_eq!(result.diagnostics, vec![]);
    assert!(is_omo_telemetry_enabled(&Value::Object(result.config)));
}

#[test]
fn telemetry_resolution_lets_the_active_profile_disable_telemetry() {
    let (_root, home, cwd) = telemetry_fixture(
        r#"{"telemetry":{"enabled":true},"profiles":{"private":{"telemetry":{"enabled":false}}}}"#,
    );
    let result = load_senpi(&home, &cwd, Some("private"));
    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(result.profile, Some("private".to_string()));
    assert!(!is_omo_telemetry_enabled(&Value::Object(result.config)));
}

#[test]
fn telemetry_resolution_defaults_to_enabled_when_no_layer_sets_it() {
    let (_root, home, cwd) = telemetry_fixture(r#"{}"#);
    let result = load_senpi(&home, &cwd, None);
    assert_eq!(result.diagnostics, vec![]);
    assert!(is_omo_telemetry_enabled(&Value::Object(result.config)));
}

#[test]
fn telemetry_resolution_rejects_an_unknown_sibling_inside_telemetry() {
    let (_root, home, cwd) =
        telemetry_fixture(r#"{"telemetry":{"enabled":false,"unexpected":true}}"#);
    let result = load_senpi(&home, &cwd, None);
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(result.diagnostics[0].kind, "validation");
    assert!(
        result.diagnostics[0]
            .issue_paths
            .contains(&"telemetry".to_string())
    );
    assert!(is_omo_telemetry_enabled(&Value::Object(result.config)));
}

#[test]
fn telemetry_resolution_rejects_a_malformed_enabled_value() {
    for content in [
        r#"{"telemetry":{"enabled":"yes"}}"#,
        r#"{"telemetry":{"enabled":1}}"#,
        r#"{"telemetry":{"enabled":null}}"#,
        r#"{"telemetry":{"enabled":{"nested":{"garbage":true}}}}"#,
    ] {
        let (_root, home, cwd) = telemetry_fixture(content);
        let result = load_senpi(&home, &cwd, None);
        assert_eq!(result.diagnostics.len(), 1, "{content}");
        assert_eq!(result.diagnostics[0].kind, "validation", "{content}");
        assert!(
            result.diagnostics[0]
                .issue_paths
                .contains(&"telemetry.enabled".to_string()),
            "{content}"
        );
        assert!(is_omo_telemetry_enabled(&Value::Object(result.config)));
    }
}

#[test]
fn characterization_of_the_top_level_only_senpi_config_shape() {
    const QUICK_PROMPT_APPEND: &str = r##"<Execution_Style>
EVAL-FIRST: `eval` is your default execution surface. Before acting, ask "how do I finish this whole step in ONE parallelized eval cell?" - then write that cell.

- One cell = one full wave: enumerate every independent lookup (file reads, `rg` searches, git queries, metadata), run them ALL concurrently (Python: `concurrent.futures.ThreadPoolExecutor` + `subprocess`; JS: `parallel(thunks)`), then filter, chain, dedupe, and aggregate INSIDE the kernel with comprehensions. Return only distilled facts, never raw dumps.
</Execution_Style>"##;

    let user_config = json!({
        "categories": {
            "quick": {
                "model": "kimi-coding/kimi-for-coding-highspeed-unlocked",
                "reasoningEffort": "minimal",
                "fallback_models": [
                    { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoningEffort": "minimal" },
                    { "model": "example-gateway/z-ai/glm-5.2-ultrafast-unlocked", "reasoningEffort": "none" },
                ],
                "prompt_append": QUICK_PROMPT_APPEND,
            },
            "deep": { "model": "quotio-openai/gpt-5.6-terra", "variant": "xhigh" },
        },
        "agents": {
            "explore": {
                "model": "kimi-coding/kimi-for-coding-highspeed",
                "models": [
                    { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoningEffort": "minimal" },
                    "example-gateway/z-ai/glm-5.2-ultrafast-unlocked",
                    { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoningEffort": "minimal" },
                ],
            },
            "oracle": { "model": "quotio-openai/gpt-5.6-sol", "reasoningEffort": "max" },
        },
    });

    let residency = std::cmp::max(
        8,
        std::thread::available_parallelism()
            .map(|value| value.get())
            .unwrap_or(1)
            * 3,
    );
    let expected = json!({
        "agents": {
            "explore": {
                "model": "kimi-coding/kimi-for-coding-highspeed",
                "models": [
                    { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoning": "minimal" },
                    "example-gateway/z-ai/glm-5.2-ultrafast-unlocked",
                    { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoning": "minimal" },
                ],
            },
            "oracle": { "model": "quotio-openai/gpt-5.6-sol", "reasoning": "max" },
        },
        "categories": {
            "quick": {
                "fallback_models": [
                    { "model": "quotio-openai/gpt-5.6-luna-fast", "reasoning": "minimal" },
                    { "model": "example-gateway/z-ai/glm-5.2-ultrafast-unlocked", "reasoning": "off" },
                ],
                "model": "kimi-coding/kimi-for-coding-highspeed-unlocked",
                "prompt_append": QUICK_PROMPT_APPEND,
                "reasoning": "minimal",
            },
            "deep": { "model": "quotio-openai/gpt-5.6-terra", "reasoning": "xhigh" },
        },
        "codegraph": { "auto_provision": true, "daemon": true, "enabled": true, "telemetry": false },
        "task": {
            "default_concurrency": 5,
            "default_execution_mode": "in-process",
            "max_depth": 1,
            "residency_max_children": residency,
            "resume_children": true,
            "team": { "max_members": 8, "max_parallel_members": 4, "max_wall_clock_minutes": 120 },
            "ttl_ms": 86_400_000,
            "wait": { "default_ms": 60_000, "max_ms": 600_000, "min_ms": 5_000 },
            "warnings": { "unavailable_categories": true },
        },
        "teams": {},
    });

    let root = tempfile::tempdir().expect("tempdir");
    let root_path = root.path().to_string_lossy().to_string();
    let home = format!("{root_path}/home");
    let cwd = format!("{home}/project");
    std::fs::create_dir_all(format!("{home}/.maho")).expect("omo dir");
    std::fs::create_dir_all(&cwd).expect("cwd");
    write_file(&format!("{home}/.maho/omo.jsonc"), &user_config.to_string());

    let result = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(cwd),
        env: Some(env_of(&[("HOME", &home)])),
        harness: Some("senpi".to_string()),
        platform: Some("linux".to_string()),
        ..empty_options()
    });

    assert_eq!(result.diagnostics, vec![]);
    assert_eq!(Value::Object(result.config), expected);
}

#[test]
fn legacy_user_config_purge_audit_finds_no_xdg_appdata_or_dot_config_branch() {
    const FORBIDDEN_TOKENS: [&str; 3] = ["XDG_CONFIG_HOME", "APPDATA", "\".config\""];
    let crate_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders: Vec<String> = Vec::new();
    for directory in ["src/loader", "src/writer"] {
        let base = crate_root.join(directory);
        let mut stack = vec![base];
        while let Some(current) = stack.pop() {
            let entries = std::fs::read_dir(&current).expect("read_dir");
            for entry in entries {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                if path.extension().and_then(|value| value.to_str()) != Some("rs") {
                    continue;
                }
                let content = std::fs::read_to_string(&path).expect("read source");
                for (index, line) in content.lines().enumerate() {
                    for token in FORBIDDEN_TOKENS {
                        if line.contains(token) {
                            offenders.push(format!(
                                "{}:{}: {}",
                                path.display(),
                                index + 1,
                                line.trim()
                            ));
                        }
                    }
                }
            }
        }
    }
    assert_eq!(offenders, Vec::<String>::new());
}
