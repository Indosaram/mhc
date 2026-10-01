//! `runners/rpc/spawn.test.ts`.

use std::collections::BTreeMap;
use std::path::{MAIN_SEPARATOR, Path};
use std::sync::Arc;

use crate::runners::rpc::spawn::{
    RpcSpawnRuntime, build_child_args, build_rpc_spawn, detect_bun_binary,
    resolve_child_session_dir, resolve_senpi_executable,
};
use crate::runners::types::RpcRunnerSpec;

const SESSION_DIR_ENV: &str = "SENPI_CODING_AGENT_SESSION_DIR";

fn base_spec() -> RpcRunnerSpec {
    RpcRunnerSpec {
        task_id: "st_1a2b3c4d".to_string(),
        cwd: "/tmp/project".to_string(),
        state_dir: "/tmp/project/.omo/senpi-task".to_string(),
        prompt: "do the work".to_string(),
        ..RpcRunnerSpec::default()
    }
}

fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn runtime(parent_env: BTreeMap<String, String>) -> RpcSpawnRuntime {
    RpcSpawnRuntime {
        is_bun_binary: false,
        exec_path: "/usr/bin/node".to_string(),
        platform: "linux".to_string(),
        parent_env,
        resolve_rpc_entry: Arc::new(|| "/rpc-entry.js".to_string()),
        resolve_senpi_executable: None,
    }
}

fn no_executable(mut runtime: RpcSpawnRuntime) -> RpcSpawnRuntime {
    runtime.resolve_senpi_executable = Some(Arc::new(|_| None));
    runtime
}

fn with_executable(mut runtime: RpcSpawnRuntime, path: &str) -> RpcSpawnRuntime {
    let path = path.to_string();
    runtime.resolve_senpi_executable = Some(Arc::new(move |_| Some(path.clone())));
    runtime
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn canonical(path: &Path) -> String {
    std::fs::canonicalize(path)
        .expect("canonical")
        .to_string_lossy()
        .into_owned()
}

fn relative_to_cwd(path: &Path) -> String {
    let cwd = std::fs::canonicalize(std::env::current_dir().expect("cwd")).expect("cwd canonical");
    let target = std::fs::canonicalize(path).expect("target canonical");
    let common = cwd
        .components()
        .zip(target.components())
        .take_while(|(left, right)| left == right)
        .count();
    let ups = cwd.components().count() - common;
    let mut relative = std::path::PathBuf::new();
    for _ in 0..ups {
        relative.push("..");
    }
    for component in target.components().skip(common) {
        relative.push(component);
    }
    relative.to_string_lossy().into_owned()
}

#[test]
fn given_a_bun_virtual_fs_url_when_detecting_then_it_reports_a_bun_binary() {
    assert!(detect_bun_binary("file:///$bunfs/root/index.js"));
    assert!(detect_bun_binary("file:///~BUN/root/index.js"));
    assert!(detect_bun_binary("file:///%7EBUN/root/index.js"));
}

#[test]
fn given_a_plain_file_url_when_detecting_then_it_is_not_a_bun_binary() {
    assert!(!detect_bun_binary("file:///Users/me/project/index.js"));
}

#[test]
fn given_a_state_dir_and_task_id_when_resolving_then_the_session_dir_nests_under_sessions_id() {
    let spec = base_spec();
    let dir = resolve_child_session_dir(&spec.state_dir, &spec.task_id);
    assert!(Path::new(&dir).is_absolute());
    let expected = Path::new(&spec.state_dir)
        .join("sessions")
        .join(&spec.task_id);
    assert!(dir.starts_with(&*expected.to_string_lossy()));
    assert!(dir.ends_with(MAIN_SEPARATOR));
}

#[test]
fn given_senpi_bin_pointing_at_an_existing_absolute_path_when_resolving_then_it_is_used_verbatim() {
    let dir = tempfile::tempdir().expect("tempdir");
    let existing = dir.path().join("senpi");
    std::fs::write(&existing, "").expect("write");
    let existing = canonical(&existing);
    let resolved = resolve_senpi_executable(&runtime(env(&[("SENPI_BIN", &existing)])));
    assert_eq!(resolved, Some(existing));
}

#[test]
fn given_senpi_bin_pointing_at_a_missing_absolute_path_when_resolving_then_it_is_null() {
    let resolved =
        resolve_senpi_executable(&runtime(env(&[("SENPI_BIN", "/definitely/missing/senpi")])));
    assert_eq!(resolved, None);
}

#[test]
fn given_a_relative_senpi_bin_when_resolving_then_the_validated_executable_is_returned_as_a_canonical_absolute_path()
 {
    let root = tempfile::tempdir().expect("tempdir");
    let executable = root.path().join("senpi");
    std::fs::write(&executable, "").expect("write");
    let override_path = relative_to_cwd(&executable);
    let resolved = resolve_senpi_executable(&runtime(env(&[("SENPI_BIN", &override_path)])))
        .expect("relative SENPI_BIN did not resolve");
    assert_eq!(resolved, canonical(&executable));
    assert!(Path::new(&resolved).is_absolute());
}

#[test]
fn given_a_relative_path_entry_when_resolving_then_the_validated_executable_is_returned_as_a_canonical_absolute_path()
 {
    let root = tempfile::tempdir().expect("tempdir");
    let executable = root.path().join("senpi");
    std::fs::write(&executable, "").expect("write");
    let path_entry = relative_to_cwd(root.path());
    let resolved = resolve_senpi_executable(&runtime(env(&[("PATH", &path_entry)])))
        .expect("relative PATH entry did not resolve");
    assert_eq!(resolved, canonical(&executable));
    assert!(Path::new(&resolved).is_absolute());
}

#[test]
fn given_no_senpi_bin_and_an_empty_path_when_resolving_a_node_runtime_then_no_executable_is_found()
{
    assert_eq!(
        resolve_senpi_executable(&runtime(env(&[("PATH", "")]))),
        None
    );
}

#[test]
fn given_a_bun_runtime_whose_sibling_senpi_binary_is_absent_when_resolving_then_it_falls_through_instead_of_returning_a_missing_path()
 {
    let mut bun = runtime(BTreeMap::new());
    bun.is_bun_binary = true;
    bun.exec_path = "/opt/senpi/bin/bun".to_string();
    assert_eq!(resolve_senpi_executable(&bun), None);
}

#[test]
fn given_a_bun_runtime_with_an_existing_sibling_senpi_binary_when_resolving_then_that_sibling_is_chosen()
 {
    let root = tempfile::tempdir().expect("tempdir");
    let sibling = root.path().join("senpi");
    std::fs::write(&sibling, "").expect("write");
    let mut bun = runtime(BTreeMap::new());
    bun.is_bun_binary = true;
    bun.exec_path = root.path().join("bun").to_string_lossy().into_owned();
    assert_eq!(resolve_senpi_executable(&bun), Some(canonical(&sibling)));
}

#[test]
fn given_a_spec_with_model_and_extensions_when_building_child_args_then_no_extensions_leads_each_e_follows_then_model()
 {
    let spec = RpcRunnerSpec {
        model: Some("omo-mock/mock-1".to_string()),
        extensions: Some(strings(&["/tmp/a.ts", "/tmp/b.ts"])),
        ..base_spec()
    };
    assert_eq!(
        build_child_args(&spec),
        strings(&[
            "--no-extensions",
            "--extension",
            "/tmp/a.ts",
            "--extension",
            "/tmp/b.ts",
            "--model",
            "omo-mock/mock-1"
        ])
    );
}

#[test]
fn given_a_spec_with_neither_model_nor_extensions_when_building_child_args_then_only_no_extensions_is_present()
 {
    assert_eq!(
        build_child_args(&base_spec()),
        strings(&["--no-extensions"])
    );
}

fn args_for_variant(model: Option<&str>, variant: &str) -> Vec<String> {
    build_child_args(&RpcRunnerSpec {
        model: model.map(str::to_string),
        variant: Some(variant.to_string()),
        ..base_spec()
    })
}

#[test]
fn given_a_spec_with_a_valid_variant_when_building_child_args_then_thinking_follows_model() {
    assert_eq!(
        args_for_variant(Some("omo-mock/mock-1"), "xhigh"),
        strings(&[
            "--no-extensions",
            "--model",
            "omo-mock/mock-1",
            "--thinking",
            "xhigh"
        ])
    );
}

#[test]
fn given_a_spec_with_high_reasoning_effort_when_building_child_args_then_it_maps_to_senpi_high() {
    assert_eq!(
        args_for_variant(Some("omo-mock/mock-1"), "high"),
        strings(&[
            "--no-extensions",
            "--model",
            "omo-mock/mock-1",
            "--thinking",
            "high"
        ])
    );
}

#[test]
fn given_the_omo_json_reasoning_effort_none_as_variant_when_building_child_args_then_it_maps_to_senpi_off()
 {
    assert_eq!(
        args_for_variant(None, "none"),
        strings(&["--no-extensions", "--thinking", "off"])
    );
}

#[test]
fn given_an_unknown_variant_when_building_child_args_then_no_thinking_flag_is_emitted() {
    assert_eq!(
        args_for_variant(Some("omo-mock/mock-1"), "ultra"),
        strings(&["--no-extensions", "--model", "omo-mock/mock-1"])
    );
}

fn windows_runtime(path: &Path) -> RpcSpawnRuntime {
    RpcSpawnRuntime {
        is_bun_binary: false,
        exec_path: "C:\\Program Files\\nodejs\\node.exe".to_string(),
        platform: "win32".to_string(),
        parent_env: env(&[("PATH", &path.to_string_lossy())]),
        resolve_rpc_entry: Arc::new(|| "/fallback/rpc-entry.js".to_string()),
        resolve_senpi_executable: None,
    }
}

fn write_package_cli(root: &Path) -> std::path::PathBuf {
    let cli = root
        .join("node_modules")
        .join("@code-yeongyu")
        .join("senpi")
        .join("dist")
        .join("cli.js");
    std::fs::create_dir_all(cli.parent().expect("cli dir")).expect("mkdir");
    std::fs::write(&cli, "").expect("write cli");
    cli
}

#[test]
fn given_a_windows_npm_senpi_installation_when_building_an_rpc_child_then_node_launches_the_npm_package_cli_without_shell_forwarding()
 {
    let npm_dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(npm_dir.path().join("senpi.cmd"), "@echo off\n").expect("shim");
    let cli = write_package_cli(npm_dir.path());
    let spec = RpcRunnerSpec {
        model: Some("omo-mock/mock-1".to_string()),
        ..base_spec()
    };
    let descriptor = build_rpc_spawn(&spec, &windows_runtime(npm_dir.path()));
    assert_eq!(descriptor.command, "C:\\Program Files\\nodejs\\node.exe");
    let mut expected = vec![canonical(&cli)];
    expected.extend(strings(&[
        "--mode",
        "rpc",
        "--no-extensions",
        "--model",
        "omo-mock/mock-1",
    ]));
    let mut actual = descriptor.args;
    actual[0] = canonical(Path::new(&actual[0]));
    assert_eq!(actual, expected);
}

#[test]
fn given_a_project_local_node_modules_bin_senpi_shim_when_building_an_rpc_child_then_node_launches_its_package_cli_without_rpc_entry_fallback()
 {
    let root = tempfile::tempdir().expect("tempdir");
    let shim_dir = root.path().join("node_modules").join(".bin");
    std::fs::create_dir_all(&shim_dir).expect("mkdir");
    std::fs::write(shim_dir.join("senpi.cmd"), "@echo off\n").expect("shim");
    let cli = write_package_cli(root.path());
    let spec = RpcRunnerSpec {
        model: Some("omo-mock/mock-1".to_string()),
        ..base_spec()
    };
    let descriptor = build_rpc_spawn(&spec, &windows_runtime(&shim_dir));
    assert_eq!(descriptor.command, "C:\\Program Files\\nodejs\\node.exe");
    assert_eq!(canonical(Path::new(&descriptor.args[0])), canonical(&cli));
    assert!(
        !descriptor
            .args
            .contains(&"/fallback/rpc-entry.js".to_string())
    );
}

#[test]
fn given_a_resolvable_senpi_executable_when_building_then_it_spawns_the_executable_in_rpc_mode() {
    let spec = RpcRunnerSpec {
        model: Some("omo-mock/mock-1".to_string()),
        extensions: Some(strings(&["/tmp/mock.ts"])),
        ..base_spec()
    };
    let descriptor = build_rpc_spawn(
        &spec,
        &with_executable(runtime(BTreeMap::new()), "/opt/homebrew/bin/senpi"),
    );
    assert_eq!(descriptor.command, "/opt/homebrew/bin/senpi");
    assert_eq!(descriptor.args[0], "--mode");
    assert_eq!(descriptor.args[1], "rpc");
    for expected in ["--model", "omo-mock/mock-1", "--extension", "/tmp/mock.ts"] {
        assert!(descriptor.args.contains(&expected.to_string()));
    }
    assert!(!descriptor.args.iter().any(|arg| arg.contains("rpc-entry")));
}

#[test]
fn given_a_bun_runtime_with_a_resolvable_sibling_executable_when_building_then_the_sibling_binary_runs_rpc_mode_with_threaded_args()
 {
    let sibling = Path::new("/opt/senpi/bin").join("senpi");
    let mut bun = with_executable(runtime(BTreeMap::new()), &sibling.to_string_lossy());
    bun.is_bun_binary = true;
    bun.exec_path = "/opt/senpi/bin/bun".to_string();
    let spec = RpcRunnerSpec {
        model: Some("omo-mock/mock-1".to_string()),
        ..base_spec()
    };
    let descriptor = build_rpc_spawn(&spec, &bun);
    assert_eq!(descriptor.command, sibling.to_string_lossy());
    assert_eq!(
        descriptor.args,
        strings(&[
            "--mode",
            "rpc",
            "--no-extensions",
            "--model",
            "omo-mock/mock-1"
        ])
    );
    assert_eq!(descriptor.cwd, base_spec().cwd);
}

#[test]
fn given_no_resolvable_executable_when_building_then_it_falls_back_to_exec_path_plus_rpc_entry_still_threading_child_args()
 {
    let mut fallback = no_executable(runtime(BTreeMap::new()));
    fallback.resolve_rpc_entry =
        Arc::new(|| "/pkg/@code-yeongyu/senpi/dist/rpc-entry.js".to_string());
    let spec = RpcRunnerSpec {
        model: Some("omo-mock/mock-1".to_string()),
        extensions: Some(strings(&["/tmp/mock.ts"])),
        ..base_spec()
    };
    let descriptor = build_rpc_spawn(&spec, &fallback);
    assert_eq!(descriptor.command, "/usr/bin/node");
    assert_eq!(
        descriptor.args,
        strings(&[
            "/pkg/@code-yeongyu/senpi/dist/rpc-entry.js",
            "--no-extensions",
            "--extension",
            "/tmp/mock.ts",
            "--model",
            "omo-mock/mock-1"
        ])
    );
}

#[test]
fn given_a_parent_env_when_building_then_the_child_gets_an_isolated_session_dir_and_inherits_parent_vars_untouched()
 {
    let parent_env = env(&[
        ("PATH", "/usr/bin"),
        ("HOME", "/Users/me"),
        ("ANTHROPIC_API_KEY", "secret"),
    ]);
    let spec = base_spec();
    let descriptor = build_rpc_spawn(&spec, &no_executable(runtime(parent_env.clone())));
    let session_dir = descriptor.env.get(SESSION_DIR_ENV).expect("session dir");
    let expected = Path::new(&spec.state_dir)
        .join("sessions")
        .join(&spec.task_id);
    assert!(session_dir.starts_with(&*expected.to_string_lossy()));
    let home = std::env::var("HOME").unwrap_or_default();
    assert!(!session_dir.starts_with(&*Path::new(&home).join(".senpi").to_string_lossy()));
    assert_eq!(
        descriptor.env.get("PATH").map(String::as_str),
        Some("/usr/bin")
    );
    assert_eq!(
        descriptor.env.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("secret")
    );
    assert_eq!(descriptor.env.get("SENPI_CODING_AGENT_DIR"), None);
    assert!(!parent_env.contains_key(SESSION_DIR_ENV));
}

#[test]
fn given_a_generic_child_spawned_by_a_member_when_building_then_member_identity_and_extension_do_not_leak()
 {
    let member_extension = "/tmp/omo-member.js";
    let provider_extension = "/tmp/provider.js";
    let spec = RpcRunnerSpec {
        extensions: Some(strings(&[member_extension, provider_extension])),
        ..base_spec()
    };
    let parent = env(&[
        ("PATH", "/usr/bin"),
        (
            "SENPI_TASK_MEMBER",
            "11111111-1111-4111-8111-111111111111::alice",
        ),
        ("SENPI_TASK_MEMBER_TASK_ID", "st_00000001"),
        ("SENPI_TASK_TEAM_CONFIG", "{\"members\":[\"alice\"]}"),
    ]);
    let descriptor = build_rpc_spawn(&spec, &no_executable(runtime(parent)));
    for name in [
        "SENPI_TASK_MEMBER",
        "SENPI_TASK_MEMBER_TASK_ID",
        "SENPI_TASK_TEAM_CONFIG",
    ] {
        assert_eq!(descriptor.env.get(name), None, "{name} leaked");
    }
    assert!(!descriptor.args.contains(&member_extension.to_string()));
    assert!(descriptor.args.contains(&provider_extension.to_string()));
}

#[test]
fn given_member_extension_env_w2mem_when_building_then_identity_config_and_task_id_reach_the_child_without_overriding_isolation()
 {
    let member_env = env(&[
        (
            "SENPI_TASK_MEMBER",
            "11111111-1111-4111-8111-111111111111::alice",
        ),
        ("SENPI_TASK_MEMBER_TASK_ID", "st_00000001"),
        ("SENPI_TASK_TEAM_CONFIG", "{\"members\":[\"alice\"]}"),
        ("SENPI_CODING_AGENT_SESSION_DIR", "/untrusted/override"),
    ]);
    let spec = RpcRunnerSpec {
        extensions: Some(strings(&["/tmp/omo-member.js"])),
        member_env: Some(member_env.clone()),
        ..base_spec()
    };
    let descriptor = build_rpc_spawn(&spec, &no_executable(runtime(env(&[("PATH", "/usr/bin")]))));
    for name in [
        "SENPI_TASK_MEMBER",
        "SENPI_TASK_MEMBER_TASK_ID",
        "SENPI_TASK_TEAM_CONFIG",
    ] {
        assert_eq!(descriptor.env.get(name), member_env.get(name));
    }
    assert_eq!(
        descriptor.env.get(SESSION_DIR_ENV),
        Some(&resolve_child_session_dir(&spec.state_dir, &spec.task_id))
    );
    assert!(descriptor.args.contains(&"/tmp/omo-member.js".to_string()));
}

#[test]
fn given_senpi_hides_rpc_entry_from_node_exports_when_building_a_fallback_spawn_then_it_uses_the_physical_dist_entry()
{
    // `spawn-node-runtime.test.ts` bundles `spawn.ts` for Node, runs the fallback under `node`, and
    // asserts the resolved entry is the physical `dist/rpc-entry.js` rather than a Node export. The
    // Rust host is not a Node process, so the same fact is pinned on the descriptor the *default*
    // runtime builds: the fallback entry is the physical dist file, not a package export.
    let fallback = RpcSpawnRuntime {
        parent_env: env(&[("PATH", "")]),
        resolve_senpi_executable: Some(Arc::new(|_| None)),
        ..RpcSpawnRuntime::default()
    };
    let spec = RpcRunnerSpec {
        task_id: "st_node_runtime".to_string(),
        cwd: std::env::current_dir()
            .expect("cwd")
            .to_string_lossy()
            .into_owned(),
        state_dir: "/tmp/st_node_runtime".to_string(),
        prompt: "READY".to_string(),
        ..RpcRunnerSpec::default()
    };
    let descriptor = build_rpc_spawn(&spec, &fallback);
    let entry = descriptor.args.first().expect("rpc entry argument");
    assert!(
        entry.ends_with(&format!("dist{MAIN_SEPARATOR}rpc-entry.js")),
        "expected the physical dist entry, got {entry}"
    );
}
