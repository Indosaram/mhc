//! Translated from ast-grep/{sg-manifest,install-script,sg-provisioner,sg-resolver,sg-probe-contract,sg-resolution}.
//! Resolver fixtures are real executable shell stubs. Zip-entry-listing tests live in sibling tests/zip_entry_listing.rs.
#![cfg(unix)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use pretty_assertions::assert_eq;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use utils::*;

fn tempdir() -> TempDir {
    TempDir::new().unwrap_or_else(|error| panic!("{error}"))
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn stub(path: &Path, body: &str, mode: u32) -> String {
    fs::create_dir_all(path.parent().unwrap_or(path)).unwrap_or_default();
    fs::write(path, body).unwrap_or_default();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap_or_default();
    text(path)
}

fn working(path: &Path) -> String {
    stub(path, "#!/bin/sh\nprintf 'ast-grep 0.45.0\\n'\n", 0o755)
}

const NO_HOME: &str = "/nonexistent-home";

fn empty_env() -> HashMap<String, String> {
    HashMap::new()
}

fn env_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

fn isolated<'a>(env: &'a HashMap<String, String>) -> SgResolverOptions<'a> {
    SgResolverOptions {
        cache: SgCacheMode::Bypass,
        env: Some(env),
        home_dir: Some(NO_HOME),
        platform: Some("linux"),
        which: Some(&|_| None),
        ..Default::default()
    }
}

fn found(resolution: &SgResolution) -> Option<(String, SgResolutionTier)> {
    match resolution {
        SgResolution::Found { path, tier } => Some((path.clone(), *tier)),
        SgResolution::NotFound { .. } => None,
    }
}

fn not_found_code(resolution: &SgResolution) -> Option<&'static str> {
    match resolution {
        SgResolution::Found { .. } => None,
        SgResolution::NotFound { error } => Some(error.code),
    }
}

fn hints(resolution: &SgResolution) -> String {
    match resolution {
        SgResolution::Found { .. } => String::new(),
        SgResolution::NotFound { error } => error.hints.join("\n"),
    }
}

#[test]
fn manifest_slugs_binary_names_and_pins() {
    assert_eq!(
        [("darwin", "arm64"), ("linux", "x64"), ("win32", "x64")].map(|(p, a)| runtime_slug(p, a)),
        ["darwin-arm64", "linux-x64", "win32-x64"]
    );
    assert_eq!(
        ["x86_64", "amd64"].map(|arch| runtime_slug("linux", arch)),
        ["linux-x64", "linux-x64"]
    );
    let asset = sg_release_asset("win32-x64").unwrap_or_else(|| panic!("missing asset"));
    assert_eq!(SG_PINNED_VERSION, "0.43.0");
    assert_eq!(sg_binary_name("win32"), "sg.exe");
    assert!(asset.url.contains("/0.43.0/"));
    assert_eq!(asset.sha256.len(), 64);
}

type Outcome = AstGrepInstallSpawnOutcome;

fn immediate(outcome: Outcome) -> AstGrepInstallSpawnedProcess {
    let (tx, rx) = mpsc::channel();
    let _ = tx.send(outcome);
    AstGrepInstallSpawnedProcess {
        kill: Box::new(|| {}),
        outcome: rx,
    }
}

fn install_options<'a>(
    platform: &'a str,
    script: &'static str,
    spawn: AstGrepInstallSpawn<'a>,
) -> AstGrepSkillInstallOptions<'a> {
    AstGrepSkillInstallOptions {
        env: None,
        file_exists: Some(Box::leak(Box::new(move |path: &str| {
            path.ends_with(script)
        }))),
        platform: Some(platform),
        skill_dir: "/skills/ast-grep".to_string(),
        spawn_process: Some(spawn),
        target_dir: "/home/test/.omo/runtime/ast-grep/linux-x64".to_string(),
        timeout_ms: None,
    }
}

#[test]
fn install_script_targets_runtime_slug_dir() {
    let target = ast_grep_runtime_dir("/home/test/.omo", "darwin", "arm64");
    let seen = RefCell::new(Vec::new());
    let spawn = |command: &str, _args: &[String], options: &AstGrepInstallSpawnOptions| {
        seen.borrow_mut().push((
            command.to_string(),
            options.env.get(AST_GREP_BIN_DIR_ENV_KEY).cloned(),
        ));
        immediate(Outcome::Exit {
            code: Some(0),
            signal: None,
        })
    };
    let mut options = install_options("darwin", "install.sh", &spawn);
    options.target_dir.clone_from(&target);
    assert_eq!(
        run_ast_grep_skill_install(&options),
        AstGrepSkillInstallResult::Succeeded
    );
    assert_eq!(
        seen.into_inner(),
        vec![("bash".to_string(), Some(target.clone()))]
    );
    assert_eq!(target, "/home/test/.omo/runtime/ast-grep/darwin-arm64");
}

#[test]
fn install_script_nonzero_exit_is_failure() {
    let spawn = |_: &str, _: &[String], _: &AstGrepInstallSpawnOptions| {
        immediate(Outcome::Exit {
            code: Some(1),
            signal: None,
        })
    };
    let result = run_ast_grep_skill_install(&install_options("linux", "install.sh", &spawn));
    assert!(matches!(result, AstGrepSkillInstallResult::Failed { .. }));
}

#[test]
fn install_script_timeout_kills_child() {
    let kills = Arc::new(Mutex::new(0));
    let counter = Arc::clone(&kills);
    let spawn = move |_: &str, _: &[String], _: &AstGrepInstallSpawnOptions| {
        let (tx, rx) = mpsc::channel();
        let counter = Arc::clone(&counter);
        AstGrepInstallSpawnedProcess {
            kill: Box::new(move || {
                *counter
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) += 1;
                let _ = tx.send(Outcome::Exit {
                    code: None,
                    signal: Some(15),
                });
            }),
            outcome: rx,
        }
    };
    let mut options = install_options("linux", "install.sh", &spawn);
    options.timeout_ms = Some(1);
    assert_eq!(
        run_ast_grep_skill_install(&options),
        AstGrepSkillInstallResult::TimedOut
    );
    assert_eq!(
        *kills
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
        1
    );
}

#[test]
fn install_script_child_ignoring_termination_fails_instead_of_hanging() {
    let holders = Mutex::new(Vec::new());
    let spawn = |_: &str, _: &[String], _: &AstGrepInstallSpawnOptions| {
        let (tx, rx) = mpsc::channel::<Outcome>();
        holders
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(tx);
        AstGrepInstallSpawnedProcess {
            kill: Box::new(|| {}),
            outcome: rx,
        }
    };
    let mut options = install_options("linux", "install.sh", &spawn);
    options.timeout_ms = Some(5);
    let started = Instant::now();
    let result = run_ast_grep_skill_install(&options);
    assert!(started.elapsed().as_secs() < 3);
    let AstGrepSkillInstallResult::Failed { reason } = result else {
        panic!("expected failure, got {result:?}")
    };
    assert!(reason.contains("ignored termination"));
}

#[test]
fn install_script_windows_falls_back_to_powershell_exe() {
    let commands = RefCell::new(Vec::new());
    let spawn = |command: &str, _: &[String], _: &AstGrepInstallSpawnOptions| {
        commands.borrow_mut().push(command.to_string());
        if command == "pwsh" {
            return immediate(Outcome::SpawnError {
                message: "missing pwsh".to_string(),
                missing_executable: true,
            });
        }
        immediate(Outcome::Exit {
            code: Some(0),
            signal: None,
        })
    };
    let mut options = install_options("win32", "install.ps1", &spawn);
    options.skill_dir = "C:\\skills\\ast-grep".to_string();
    assert_eq!(
        run_ast_grep_skill_install(&options),
        AstGrepSkillInstallResult::Succeeded
    );
    assert_eq!(commands.into_inner(), vec!["pwsh", "powershell.exe"]);
}

fn fixture_zip(entry_name: &str, bytes: &[u8]) -> Vec<u8> {
    let name = entry_name.as_bytes();
    let size = u32::try_from(bytes.len()).unwrap_or_default().to_le_bytes();
    let name_len = u16::try_from(name.len()).unwrap_or_default().to_le_bytes();
    let mut local = Vec::new();
    local.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
    local.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    local.extend_from_slice(&[0, 0, 0, 0]);
    local.extend_from_slice(&size);
    local.extend_from_slice(&size);
    local.extend_from_slice(&name_len);
    local.extend_from_slice(&[0, 0]);
    local.extend_from_slice(name);
    let mut central = Vec::new();
    central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
    central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    central.extend_from_slice(&[0, 0, 0, 0]);
    central.extend_from_slice(&size);
    central.extend_from_slice(&size);
    central.extend_from_slice(&name_len);
    central.extend_from_slice(&[0; 12]);
    central.extend_from_slice(&[0, 0, 0, 0]);
    central.extend_from_slice(name);
    let central_offset = u32::try_from(local.len() + bytes.len()).unwrap_or_default();
    let mut zip = local;
    zip.extend_from_slice(bytes);
    let central_len = u32::try_from(central.len()).unwrap_or_default();
    zip.extend_from_slice(&central);
    zip.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    zip.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    zip.extend_from_slice(&central_len.to_le_bytes());
    zip.extend_from_slice(&central_offset.to_le_bytes());
    zip.extend_from_slice(&[0, 0]);
    zip
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn leak_asset(sha256: String, url: &'static str) -> SgManifestAsset {
    SgManifestAsset {
        sha256: Box::leak(sha256.into_boxed_str()),
        url,
    }
}

#[test]
fn provision_downloads_verifies_and_writes_sg() {
    let dir = tempdir();
    let binary = b"#!/bin/sh\nprintf 'ast-grep 0.43.0\\n'\n".to_vec();
    let archive = fixture_zip("release/ast-grep", &binary);
    let assets = [(
        "linux-x64",
        leak_asset(sha(&archive), "memory://ast-grep.zip"),
    )];
    let fetch = |_: &str| {
        Ok(SgFetchResponse {
            status: 200,
            body: archive.clone(),
        })
    };
    let options = SgProvisionOptions {
        arch: Some("x64"),
        fetch: &fetch,
        platform: Some("linux"),
        release_assets: Some(&assets),
        target_dir: dir.path().to_path_buf(),
    };
    let result = provision_sg_binary(&options).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(result, dir.path().join("sg"));
    assert_eq!(fs::read(&result).unwrap_or_default(), binary);
}

#[test]
fn provision_network_failure_is_actionable() {
    let dir = tempdir();
    let fetch = |_: &str| Err("offline".to_string());
    let options = SgProvisionOptions {
        arch: None,
        fetch: &fetch,
        platform: Some("linux"),
        release_assets: None,
        target_dir: dir.path().to_path_buf(),
    };
    let error = provision_sg_binary(&options)
        .err()
        .unwrap_or_else(|| panic!("expected error"));
    assert_eq!(error.code, SgProvisionErrorCode::DownloadFailed);
    assert!(error.message.contains("failed to download ast-grep"));
    assert!(!dir.path().join("sg").exists());
}

#[test]
fn provision_bad_sha_leaves_no_binary() {
    let dir = tempdir();
    let archive = b"not the pinned release archive".to_vec();
    let expected = sha(&archive);
    let fetch = |_: &str| {
        Ok(SgFetchResponse {
            status: 200,
            body: archive.clone(),
        })
    };
    let options = SgProvisionOptions {
        arch: None,
        fetch: &fetch,
        platform: Some("linux"),
        release_assets: None,
        target_dir: dir.path().to_path_buf(),
    };
    let error = provision_sg_binary(&options)
        .err()
        .unwrap_or_else(|| panic!("expected error"));
    assert!(error.message.contains(&format!("got {expected}")));
    assert!(!dir.path().join("sg").exists());
}

#[test]
fn provision_keeps_writes_inside_target_dir() {
    let root = tempdir();
    let target = root.path().join("target");
    let binary = b"standalone ast-grep".to_vec();
    let archive = fixture_zip("ast-grep.exe", &binary);
    let assets = [(
        "win32-x64",
        leak_asset(sha(&archive), "memory://ast-grep-windows.zip"),
    )];
    let fetch = |_: &str| {
        Ok(SgFetchResponse {
            status: 200,
            body: archive.clone(),
        })
    };
    let options = SgProvisionOptions {
        arch: Some("x64"),
        fetch: &fetch,
        platform: Some("win32"),
        release_assets: Some(&assets),
        target_dir: target.clone(),
    };
    let result = provision_sg_binary(&options).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(result, target.join("sg.exe"));
    assert!(!root.path().join("sg.exe").exists());
    assert_eq!(fs::read(&result).unwrap_or_default(), binary);
}

fn ok_probe(_: &str) -> Result<String, String> {
    Ok("ast-grep 0.43.0".to_string())
}

#[test]
fn resolver_prefers_env_override() {
    let dir = tempdir();
    let override_path = working(&dir.path().join("custom-sg"));
    let env = env_of(&[(SG_PATH_ENV_KEY, &override_path)]);
    let runtime = text(&dir.path().join("runtime"));
    let path_sg = text(&dir.path().join("path-sg"));
    let exists = |path: &str| path == override_path;
    let which = |_: &str| Some(path_sg.clone());
    let options = SgResolverOptions {
        env: Some(&env),
        file_exists: Some(&exists),
        run_version_probe: Some(&ok_probe),
        runtime_dir: Some(&runtime),
        which: Some(&which),
        cache: SgCacheMode::Bypass,
        ..Default::default()
    };
    assert_eq!(find_sg_binary_sync(&options), Some(override_path.clone()));
}

#[test]
fn resolver_runtime_dir_before_path() {
    let dir = tempdir();
    let runtime = text(&dir.path().join("runtime"));
    let runtime_sg = working(&dir.path().join("runtime/sg"));
    let env = empty_env();
    let exists = |path: &str| path == runtime_sg;
    let path_sg = text(&dir.path().join("path-sg"));
    let which = |_: &str| Some(path_sg.clone());
    let options = SgResolverOptions {
        file_exists: Some(&exists),
        run_version_probe: Some(&ok_probe),
        runtime_dir: Some(&runtime),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(find_sg_binary_sync(&options), Some(runtime_sg.clone()));
}

#[test]
fn resolver_returns_none_when_nothing_resolves() {
    let env = empty_env();
    let options = SgResolverOptions {
        file_exists: Some(&|_| false),
        run_version_probe: Some(&|_| Err("should not matter".to_string())),
        ..isolated(&env)
    };
    assert_eq!(find_sg_binary_sync(&options), None);
}

fn assert_ast_grep_preferred(platform: &str, ast_grep: &str, sg_alias: &str) {
    let probed = RefCell::new(Vec::new());
    let env = empty_env();
    let exists = |path: &str| path == ast_grep || path == sg_alias;
    let probe = |path: &str| {
        probed.borrow_mut().push(path.to_string());
        if path == sg_alias {
            return Err("the sg alias probe must never run".to_string());
        }
        Ok("ast-grep 0.45.0".to_string())
    };
    let which = |name: &str| {
        Some(if name == "ast-grep" {
            ast_grep.to_string()
        } else {
            sg_alias.to_string()
        })
    };
    let options = SgResolverOptions {
        file_exists: Some(&exists),
        platform: Some(platform),
        run_version_probe: Some(&probe),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(find_sg_binary_sync(&options), Some(ast_grep.to_string()));
    assert_eq!(probed.into_inner(), vec![ast_grep.to_string()]);
}

#[test]
fn resolver_prefers_ast_grep_over_sg_on_darwin() {
    assert_ast_grep_preferred("darwin", "/usr/local/bin/ast-grep", "/usr/local/bin/sg");
}

#[test]
fn resolver_prefers_ast_grep_over_sg_on_win32() {
    assert_ast_grep_preferred("win32", "C:\\tools\\ast-grep.exe", "C:\\tools\\sg.exe");
}

#[test]
fn resolver_falls_back_to_sg_alias_on_darwin() {
    let sg_alias = "/opt/homebrew/bin/sg";
    let env = empty_env();
    let exists = |path: &str| path == sg_alias;
    let which = |name: &str| (name == "sg").then(|| sg_alias.to_string());
    let options = SgResolverOptions {
        file_exists: Some(&exists),
        platform: Some("darwin"),
        run_version_probe: Some(&ok_probe),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(find_sg_binary_sync(&options), Some(sg_alias.to_string()));
}

#[test]
fn resolver_default_probe_discards_child_stderr() {
    let dir = tempdir();
    let fake = stub(
        &dir.path().join("sg"),
        "#!/bin/sh\nprintf 'WARNING: sg is deprecated\\n' >&2\nprintf 'ast-grep 0.45.0\\n'\n",
        0o755,
    );
    let script = dir.path().join("probe.sh");
    stub(
        &script,
        &format!("#!/bin/sh\n{fake} --version 2>/dev/null\n"),
        0o755,
    );
    let env = empty_env();
    let which = |name: &str| (name == "sg").then(|| fake.clone());
    let options = SgResolverOptions {
        platform: Some("darwin"),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(find_sg_binary_sync(&options), Some(fake.clone()));
}

#[test]
fn resolver_prefers_ast_grep_over_linux_setgroups() {
    let setgroups = "/usr/bin/sg";
    let ast_grep = "/usr/local/bin/ast-grep";
    let env = empty_env();
    let exists = |path: &str| path == setgroups || path == ast_grep;
    let probe = |path: &str| {
        Ok(if path == setgroups {
            "sg from shadow-utils"
        } else {
            "ast-grep 0.43.0"
        }
        .to_string())
    };
    let which = |name: &str| {
        Some(
            if name == "ast-grep" {
                ast_grep
            } else {
                setgroups
            }
            .to_string(),
        )
    };
    let options = SgResolverOptions {
        file_exists: Some(&exists),
        run_version_probe: Some(&probe),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(find_sg_binary_sync(&options), Some(ast_grep.to_string()));
}

#[test]
fn contract_broken_override_loses_to_path() {
    let dir = tempdir();
    let broken = stub(
        &dir.path().join("override/custom-override"),
        "#!/bin/sh\nexit 42\n",
        0o755,
    );
    let path_ast_grep = working(&dir.path().join("path/ast-grep"));
    let env = env_of(&[(SG_PATH_ENV_KEY, &broken)]);
    let which = |name: &str| (name == "ast-grep").then(|| path_ast_grep.clone());
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        which: Some(&which),
        ..isolated(&env)
    });
    assert_eq!(
        found(&result),
        Some((path_ast_grep.clone(), SgResolutionTier::Path))
    );
}

#[test]
fn contract_broken_override_alone_is_not_found() {
    let dir = tempdir();
    let broken = stub(
        &dir.path().join("override/custom-override"),
        "#!/bin/sh\nexit 42\n",
        0o755,
    );
    let env = env_of(&[(SG_PATH_ENV_KEY, &broken)]);
    assert_eq!(
        not_found_code(&resolve_sg_binary_sync(&isolated(&env))),
        Some(SG_BINARY_NOT_FOUND)
    );
}

#[test]
fn contract_revalidation_rejects_binary_that_lost_exec_bit() {
    let dir = tempdir();
    let override_path = working(&dir.path().join("override/sg"));
    let env = env_of(&[(SG_PATH_ENV_KEY, &override_path)]);
    clear_sg_resolution_cache();
    let options = SgResolverOptions {
        cache: SgCacheMode::Use,
        ..isolated(&env)
    };
    let cached = resolve_sg_binary_sync(&options);
    fs::set_permissions(&override_path, fs::Permissions::from_mode(0o644)).unwrap_or_default();
    let revalidated = resolve_sg_binary_sync(&SgResolverOptions {
        revalidate: true,
        cache: SgCacheMode::Use,
        ..isolated(&env)
    });
    clear_sg_resolution_cache();
    assert_eq!(
        found(&cached).map(|(path, _)| path),
        Some(override_path.clone())
    );
    assert_eq!(not_found_code(&revalidated), Some(SG_BINARY_NOT_FOUND));
}

#[test]
fn contract_non_executable_candidate_is_rejected() {
    let dir = tempdir();
    let unreadable = stub(
        &dir.path().join("override/ast-grep"),
        "#!/bin/sh\nprintf 'ast-grep 0.45.0\\n'\n",
        0o644,
    );
    let path_ast_grep = working(&dir.path().join("path/ast-grep"));
    let env = env_of(&[(SG_PATH_ENV_KEY, &unreadable)]);
    let which = |name: &str| (name == "ast-grep").then(|| path_ast_grep.clone());
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        which: Some(&which),
        ..isolated(&env)
    });
    assert_eq!(
        found(&result).map(|(path, _)| path),
        Some(path_ast_grep.clone())
    );
}

#[test]
fn contract_hanging_probe_is_bounded_and_skipped() {
    let dir = tempdir();
    let hanging = stub(
        &dir.path().join("path/ast-grep"),
        "#!/bin/sh\nsleep 60\n",
        0o755,
    );
    let working_sg = working(&dir.path().join("path/sg"));
    let env = empty_env();
    let which = |name: &str| {
        Some(if name == "ast-grep" {
            hanging.clone()
        } else {
            working_sg.clone()
        })
    };
    let started = Instant::now();
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        which: Some(&which),
        ..isolated(&env)
    });
    let elapsed = started.elapsed().as_millis();
    assert_eq!(
        found(&result).map(|(path, _)| path),
        Some(working_sg.clone())
    );
    assert!((4_000..20_000).contains(&elapsed), "{elapsed}ms");
}

#[test]
fn contract_every_probe_throwing_is_not_found() {
    let dir = tempdir();
    let override_path = text(&dir.path().join("override/ast-grep"));
    let runtime = text(&dir.path().join("runtime"));
    let path_ast_grep = text(&dir.path().join("path/ast-grep"));
    let env = env_of(&[(SG_PATH_ENV_KEY, &override_path)]);
    let which = |name: &str| (name == "ast-grep").then(|| path_ast_grep.clone());
    let options = SgResolverOptions {
        file_exists: Some(&|_| true),
        runtime_dir: Some(&runtime),
        run_version_probe: Some(&|_| Err("probe cannot run".to_string())),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(
        not_found_code(&resolve_sg_binary_sync(&options)),
        Some(SG_BINARY_NOT_FOUND)
    );
}

#[test]
fn contract_name_grants_no_exemption() {
    let dir = tempdir();
    let impostor = stub(
        &dir.path().join("path/ast-grep"),
        "#!/bin/sh\nprintf 'ripgrep 14.1.0\\n'\n",
        0o755,
    );
    let env = empty_env();
    let which = |name: &str| (name == "ast-grep").then(|| impostor.clone());
    assert_eq!(
        found(&resolve_sg_binary_sync(&SgResolverOptions {
            which: Some(&which),
            ..isolated(&env)
        })),
        None
    );
}

#[test]
fn contract_runtime_location_grants_no_exemption() {
    let dir = tempdir();
    let runtime = text(&dir.path().join("runtime"));
    stub(&dir.path().join("runtime/sg"), "#!/bin/sh\nexit 3\n", 0o755);
    let path_ast_grep = working(&dir.path().join("path/ast-grep"));
    let env = empty_env();
    let which = |name: &str| (name == "ast-grep").then(|| path_ast_grep.clone());
    let options = SgResolverOptions {
        runtime_dir: Some(&runtime),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)).map(|(path, _)| path),
        Some(path_ast_grep.clone())
    );
}

#[test]
fn contract_package_dir_activates_skill_bin_tier() {
    let dir = tempdir();
    let package_dir = text(&dir.path().join("ast-grep-mcp"));
    let package_local = working(&dir.path().join("ast-grep-mcp/bin/ast-grep"));
    let path_ast_grep = working(&dir.path().join("path/ast-grep"));
    let env = empty_env();
    let which = |name: &str| (name == "ast-grep").then(|| path_ast_grep.clone());
    let options = SgResolverOptions {
        package_dir: Some(&package_dir),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)),
        Some((package_local.clone(), SgResolutionTier::SkillBin))
    );
}

fn recording(outputs: HashMap<String, String>) -> (RefCell<Vec<String>>, HashMap<String, String>) {
    (RefCell::new(Vec::new()), outputs)
}

#[test]
fn tiers_env_override_wins_and_probes_once() {
    let dir = tempdir();
    let env_path = working(&dir.path().join("env/sg"));
    let runtime_path = working(&dir.path().join("runtime/sg"));
    let runtime = text(&dir.path().join("runtime"));
    let (calls, outputs) = recording(HashMap::from([
        (env_path.clone(), "ast-grep 0.43.0".to_string()),
        (runtime_path, "ast-grep 0.43.0".to_string()),
    ]));
    let probe = |path: &str| {
        calls.borrow_mut().push(path.to_string());
        outputs
            .get(path)
            .cloned()
            .ok_or_else(|| "no stub".to_string())
    };
    let env = env_of(&[(SG_PATH_ENV_KEY, &env_path)]);
    let options = SgResolverOptions {
        runtime_dir: Some(&runtime),
        run_version_probe: Some(&probe),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)),
        Some((env_path.clone(), SgResolutionTier::EnvOverride))
    );
    assert_eq!(calls.into_inner(), vec![env_path.clone()]);
}

#[test]
fn tiers_codex_home_runtime() {
    let dir = tempdir();
    let codex = text(&dir.path().join("codex"));
    let runtime_path = working(&dir.path().join("codex/runtime/ast-grep/linux-x64/sg"));
    let env = env_of(&[("CODEX_HOME", &codex)]);
    let options = SgResolverOptions {
        arch: Some("x64"),
        run_version_probe: Some(&ok_probe),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)),
        Some((runtime_path.clone(), SgResolutionTier::OmoRuntime))
    );
}

#[test]
fn tiers_home_runtime_slug_dir() {
    let dir = tempdir();
    let home = text(dir.path());
    let runtime_path = working(&dir.path().join(".maho/runtime/ast-grep/darwin-arm64/sg"));
    let env = empty_env();
    let options = SgResolverOptions {
        arch: Some("arm64"),
        home_dir: Some(&home),
        platform: Some("darwin"),
        run_version_probe: Some(&ok_probe),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)),
        Some((runtime_path.clone(), SgResolutionTier::OmoRuntime))
    );
}

#[test]
fn tiers_skill_bin_before_path() {
    let dir = tempdir();
    let bin = text(&dir.path().join("skill-bin"));
    let cached = working(&dir.path().join("skill-bin/sg"));
    let path_sg = working(&dir.path().join("path/sg"));
    let calls = RefCell::new(Vec::new());
    let probe = |path: &str| {
        calls.borrow_mut().push(path.to_string());
        Ok("ast-grep 0.43.0".to_string())
    };
    let env = env_of(&[(AST_GREP_BIN_DIR_ENV_KEY, &bin)]);
    let which = |_: &str| Some(path_sg.clone());
    let options = SgResolverOptions {
        run_version_probe: Some(&probe),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)),
        Some((cached.clone(), SgResolutionTier::SkillBin))
    );
    assert_eq!(calls.into_inner(), vec![cached.clone()]);
}

#[test]
fn tiers_package_bin_covers_sg_alias() {
    let dir = tempdir();
    let package = text(&dir.path().join("package"));
    let cached = working(&dir.path().join("package/bin/sg"));
    let env = empty_env();
    let options = SgResolverOptions {
        package_dir: Some(&package),
        run_version_probe: Some(&ok_probe),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)),
        Some((cached.clone(), SgResolutionTier::SkillBin))
    );
}

fn only_file(
    expected: &'static str,
    platform: &'static str,
) -> (Option<(String, SgResolutionTier)>, Vec<String>) {
    let probed = RefCell::new(Vec::new());
    let env = empty_env();
    let exists = |path: &str| path == expected;
    let probe = |path: &str| {
        probed.borrow_mut().push(path.to_string());
        Ok("ast-grep 0.43.0".to_string())
    };
    let options = SgResolverOptions {
        file_exists: Some(&exists),
        platform: Some(platform),
        run_version_probe: Some(&probe),
        ..isolated(&env)
    };
    let result = found(&resolve_sg_binary_sync(&options));
    (result, probed.into_inner())
}

#[test]
fn tiers_homebrew_last() {
    let (result, probed) = only_file("/opt/homebrew/bin/sg", "darwin");
    assert_eq!(
        result,
        Some((
            "/opt/homebrew/bin/sg".to_string(),
            SgResolutionTier::Homebrew
        ))
    );
    assert_eq!(probed, vec!["/opt/homebrew/bin/sg"]);
}

#[test]
fn tiers_linuxbrew_only_on_linux() {
    let (result, probed) = only_file("/home/linuxbrew/.linuxbrew/bin/sg", "linux");
    assert_eq!(
        result.map(|(path, _)| path),
        Some("/home/linuxbrew/.linuxbrew/bin/sg".to_string())
    );
    assert_eq!(probed, vec!["/home/linuxbrew/.linuxbrew/bin/sg"]);
}

#[test]
fn probe_failure_falls_through_to_next_tier() {
    let dir = tempdir();
    let runtime = text(&dir.path().join("runtime"));
    let broken = stub(
        &dir.path().join("runtime/sg"),
        "#!/bin/sh\nprintf 'not the tool\\n'\n",
        0o755,
    );
    let path_sg = working(&dir.path().join("path/sg"));
    let calls = RefCell::new(Vec::new());
    let outputs = HashMap::from([
        (broken.clone(), "some other sg 1.0"),
        (path_sg.clone(), "ast-grep 0.43.0"),
    ]);
    let probe = |path: &str| {
        calls.borrow_mut().push(path.to_string());
        outputs
            .get(path)
            .map(|out| (*out).to_string())
            .ok_or_else(|| "no stub".to_string())
    };
    let env = empty_env();
    let which = |name: &str| (name == "sg").then(|| path_sg.clone());
    let options = SgResolverOptions {
        runtime_dir: Some(&runtime),
        run_version_probe: Some(&probe),
        which: Some(&which),
        ..isolated(&env)
    };
    assert_eq!(
        found(&resolve_sg_binary_sync(&options)).map(|(path, _)| path),
        Some(path_sg.clone())
    );
    assert_eq!(calls.into_inner(), vec![broken, path_sg]);
}

#[test]
fn probe_rejects_non_ast_grep_override() {
    let dir = tempdir();
    let impostor = stub(
        &dir.path().join("impostor/sg"),
        "#!/bin/sh\nprintf 'sg 1.0\\n'\n",
        0o755,
    );
    let path_ast_grep = working(&dir.path().join("path/ast-grep"));
    let env = env_of(&[(SG_PATH_ENV_KEY, &impostor)]);
    let which = |name: &str| (name == "ast-grep").then(|| path_ast_grep.clone());
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        which: Some(&which),
        ..isolated(&env)
    });
    assert_eq!(
        found(&result).map(|(path, _)| path),
        Some(path_ast_grep.clone())
    );
}

fn setgroups(path: &Path) -> String {
    stub(
        path,
        "#!/bin/sh\nprintf 'sg from shadow-utils 4.13\\n' >&2\nexit 1\n",
        0o755,
    )
}

#[test]
fn probe_rejects_setgroups_impostor_alone() {
    let dir = tempdir();
    let impostor = setgroups(&dir.path().join("usr-bin/sg"));
    let env = empty_env();
    let which = |name: &str| (name == "sg").then(|| impostor.clone());
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        which: Some(&which),
        ..isolated(&env)
    });
    assert_eq!(not_found_code(&result), Some(SG_BINARY_NOT_FOUND));
}

#[test]
fn probe_contains_real_setgroups_failure_and_continues() {
    let dir = tempdir();
    let impostor = setgroups(&dir.path().join("usr-bin/sg"));
    let real = working(&dir.path().join("brew/ast-grep"));
    let env = empty_env();
    let which = |name: &str| match name {
        "sg" => Some(impostor.clone()),
        "ast-grep" => Some(real.clone()),
        _ => None,
    };
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        which: Some(&which),
        ..isolated(&env)
    });
    assert_eq!(found(&result).map(|(path, _)| path), Some(real.clone()));
}

#[test]
fn probe_garbage_candidates_never_resolve() {
    let dir = tempdir();
    let dir_named_sg = dir.path().join("path/sg");
    fs::create_dir_all(&dir_named_sg).unwrap_or_default();
    fs::create_dir_all(dir.path().join("bin")).unwrap_or_default();
    fs::write(dir.path().join("bin/ast-grep"), "").unwrap_or_default();
    let bin = text(&dir.path().join("bin"));
    let missing = text(&dir.path().join("does-not-exist"));
    let env = env_of(&[
        (AST_GREP_BIN_DIR_ENV_KEY, &bin),
        (SG_PATH_ENV_KEY, &missing),
    ]);
    let dir_text = text(&dir_named_sg);
    let which = |name: &str| (name == "sg").then(|| dir_text.clone());
    assert_eq!(
        found(&resolve_sg_binary_sync(&SgResolverOptions {
            which: Some(&which),
            ..isolated(&env)
        })),
        None
    );
}

#[test]
fn probe_hang_is_bounded_and_reports_not_found() {
    let dir = tempdir();
    let hanging = stub(&dir.path().join("bin/sg"), "#!/bin/sh\nsleep 60\n", 0o755);
    let env = empty_env();
    let which = |name: &str| (name == "sg").then(|| hanging.clone());
    let started = Instant::now();
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        which: Some(&which),
        ..isolated(&env)
    });
    let elapsed = started.elapsed().as_millis();
    assert_eq!(found(&result), None);
    assert!((4_000..20_000).contains(&elapsed), "{elapsed}ms");
}

#[test]
fn cache_hit_skips_reprobe() {
    let dir = tempdir();
    let bin = text(&dir.path().join("bin"));
    let cached = working(&dir.path().join("bin/sg"));
    let calls = RefCell::new(Vec::new());
    let probe = |path: &str| {
        calls.borrow_mut().push(path.to_string());
        Ok("ast-grep 0.43.0".to_string())
    };
    let env = env_of(&[(AST_GREP_BIN_DIR_ENV_KEY, &bin)]);
    clear_sg_resolution_cache();
    let options = SgResolverOptions {
        cache: SgCacheMode::Use,
        run_version_probe: Some(&probe),
        ..isolated(&env)
    };
    let first = find_sg_binary_sync(&options);
    let second = find_sg_binary_sync(&options);
    clear_sg_resolution_cache();
    assert_eq!(
        (first, second),
        (Some(cached.clone()), Some(cached.clone()))
    );
    assert_eq!(calls.into_inner(), vec![cached]);
}

#[test]
fn cache_invalidated_when_binary_disappears() {
    let dir = tempdir();
    let bin = text(&dir.path().join("bin"));
    let cached = working(&dir.path().join("bin/sg"));
    let replacement = working(&dir.path().join("path/ast-grep"));
    let env = env_of(&[(AST_GREP_BIN_DIR_ENV_KEY, &bin)]);
    clear_sg_resolution_cache();
    let first = find_sg_binary_sync(&SgResolverOptions {
        cache: SgCacheMode::Use,
        ..isolated(&env)
    });
    fs::remove_dir_all(dir.path().join("bin")).unwrap_or_default();
    let empty = empty_env();
    let which = |name: &str| (name == "ast-grep").then(|| replacement.clone());
    let second = find_sg_binary_sync(&SgResolverOptions {
        cache: SgCacheMode::Use,
        which: Some(&which),
        ..isolated(&empty)
    });
    clear_sg_resolution_cache();
    assert_eq!((first, second), (Some(cached), Some(replacement.clone())));
}

#[test]
fn cache_dropped_when_revalidation_probe_fails() {
    let dir = tempdir();
    let bin = text(&dir.path().join("bin"));
    let cached = working(&dir.path().join("bin/sg"));
    let replacement = working(&dir.path().join("path/ast-grep"));
    let failing = RefCell::new(false);
    let probe = |path: &str| {
        if path == cached && *failing.borrow() {
            return Err("spawn ENOENT".to_string());
        }
        Ok("ast-grep 0.43.0".to_string())
    };
    let env = env_of(&[(AST_GREP_BIN_DIR_ENV_KEY, &bin)]);
    let which = |name: &str| (name == "ast-grep").then(|| replacement.clone());
    clear_sg_resolution_cache();
    let base = || SgResolverOptions {
        cache: SgCacheMode::Use,
        run_version_probe: Some(&probe),
        which: Some(&which),
        ..isolated(&env)
    };
    let first = find_sg_binary_sync(&base());
    *failing.borrow_mut() = true;
    let second = find_sg_binary_sync(&SgResolverOptions {
        revalidate: true,
        ..base()
    });
    clear_sg_resolution_cache();
    assert_eq!(
        (first, second),
        (Some(cached.clone()), Some(replacement.clone()))
    );
}

fn failure_hints(platform: &str) -> String {
    let env = empty_env();
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        file_exists: Some(&|_| false),
        platform: Some(platform),
        ..isolated(&env)
    });
    assert_eq!(not_found_code(&result), Some(SG_BINARY_NOT_FOUND));
    hints(&result)
}

#[test]
fn failure_hints_per_platform() {
    let darwin = failure_hints("darwin");
    assert!(
        darwin.contains("brew install ast-grep") && darwin.contains("npm install -g @ast-grep/cli")
    );
    let linux = failure_hints("linux");
    assert!(
        linux.contains("npm install -g @ast-grep/cli")
            && linux.contains("cargo install ast-grep --locked")
    );
    assert!(linux.to_lowercase().contains("omo"));
    let windows = failure_hints("win32").to_lowercase();
    for needle in [
        "scoop install",
        "winget install",
        "choco install",
        "npm install -g @ast-grep/cli",
    ] {
        assert!(windows.contains(needle), "{needle}");
    }
}

#[test]
fn failure_message_mentions_ast_grep() {
    let env = empty_env();
    let result = resolve_sg_binary_sync(&SgResolverOptions {
        file_exists: Some(&|_| false),
        platform: Some("darwin"),
        ..isolated(&env)
    });
    let SgResolution::NotFound { error } = result else {
        panic!("expected not found")
    };
    assert!(error.message.contains("ast-grep"));
}

#[test]
fn win32_candidates_use_exe_suffix() {
    let probed = RefCell::new(Vec::new());
    let env = env_of(&[
        (AST_GREP_BIN_DIR_ENV_KEY, "C:\\cache"),
        ("CODEX_HOME", "C:\\codex"),
    ]);
    let exists = |path: &str| {
        probed.borrow_mut().push(path.replace('\\', "/"));
        false
    };
    let options = SgResolverOptions {
        arch: Some("x64"),
        file_exists: Some(&exists),
        platform: Some("win32"),
        ..isolated(&env)
    };
    assert_eq!(found(&resolve_sg_binary_sync(&options)), None);
    let probed = probed.into_inner();
    for expected in [
        "C:/codex/runtime/ast-grep/win32-x64/sg.exe",
        "C:/cache/ast-grep.exe",
        "C:/cache/sg.exe",
    ] {
        assert!(
            probed.iter().any(|path| path == expected),
            "{expected} not in {probed:?}"
        );
    }
}

#[test]
fn find_sg_binary_compat_returns_plain_path_or_none() {
    let dir = tempdir();
    let bin = text(&dir.path().join("bin"));
    let cached = working(&dir.path().join("bin/sg"));
    let env = env_of(&[(AST_GREP_BIN_DIR_ENV_KEY, &bin)]);
    let native = SgResolverOptions {
        platform: None,
        ..isolated(&env)
    };
    assert_eq!(find_sg_binary_sync(&native), Some(cached));
    let empty = empty_env();
    assert_eq!(
        find_sg_binary_sync(&SgResolverOptions {
            file_exists: Some(&|_| false),
            ..isolated(&empty)
        }),
        None
    );
}
