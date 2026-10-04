use super::*;
use crate::settings_manager::InMemorySettingsStorage;
use serde_json::json;
use std::sync::Mutex;

#[derive(Default)]
struct FakeRunner {
    commands: Mutex<Vec<PackageCommand>>,
    fail: bool,
    latest: Option<String>,
    global_root: Option<String>,
}

impl FakeRunner {
    fn execute(&self, command: PackageCommand) -> PackageResult<String> {
        self.commands.lock().expect("commands").push(command.clone());
        if self.fail { return Err(PackageManagerError::Message("fake failure".into())); }
        let args = &command.args;
        if args.first().is_some_and(|arg| arg == "clone") {
            let target = args.last().expect("clone target");
            std::fs::create_dir_all(join_path(target, "prompts"))?;
            std::fs::write(join_path(target, "prompts/git.md"), "git prompt")?;
        }
        if args.first().is_some_and(|arg| arg == "view") { return Ok(self.latest.clone().unwrap_or_else(|| "\"2.0.0\"".into())); }
        if args.first().is_some_and(|arg| arg == "root") { return Ok(self.global_root.clone().unwrap_or_default()); }
        if args.first().is_some_and(|arg| arg == "rev-parse") {
            return Ok(if args.get(1).is_some_and(|arg| arg == "--abbrev-ref") { "origin/main" } else { "same-head" }.into());
        }
        if args.first().is_some_and(|arg| arg == "install") {
            if let Some(offset) = args.iter().position(|arg| arg == "--prefix" || arg == "--cwd") {
                let root = &args[offset + 1];
                for spec in &args[1..offset] {
                    if let ParsedSource::Npm { name, version, .. } = parse_source(&format!("npm:{spec}")) {
                        let path = join_path(root, &format!("node_modules/{name}"));
                        std::fs::create_dir_all(join_path(&path, "prompts"))?;
                        let version = version.filter(|version| parse_version(version).is_some()).unwrap_or_else(|| "2.0.0".into());
                        std::fs::write(join_path(&path, "package.json"), json!({"version":version}).to_string())?;
                        std::fs::write(join_path(&path, "prompts/npm.md"), "npm prompt")?;
                    }
                }
            }
        }
        if args.first().is_some_and(|arg| arg == "uninstall") && let Some(offset) = args.iter().position(|arg| arg == "--prefix" || arg == "--cwd") {
            remove_dir_if_exists(&join_path(&args[offset + 1], &format!("node_modules/{}", args[1])))?;
        }
        Ok(String::new())
    }

    fn calls(&self) -> Vec<PackageCommand> { self.commands.lock().expect("commands").iter().filter(|command| command.program != "setfattr" && command.program != "xattr").cloned().collect() }
}

#[async_trait::async_trait]
impl PackageProcessRunner for FakeRunner {
    async fn run(&self, command: PackageCommand) -> PackageResult<String> { self.execute(command) }
    fn run_sync(&self, command: PackageCommand) -> PackageResult<String> { self.execute(command) }
}

fn settings(trusted: bool) -> SettingsManager { SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), trusted) }

fn manager<'a>(dir: &tempfile::TempDir, settings: &'a mut SettingsManager, runner: Arc<FakeRunner>) -> DefaultPackageManager<'a> {
    let mut manager = DefaultPackageManager::with_runner(PackageManagerOptions {
        cwd: &dir.path().to_string_lossy(), agent_dir: &dir.path().join("agent").to_string_lossy(), settings_manager: settings,
    }, runner);
    manager.env.clear();
    manager.home_dir = dir.path().join("home").to_string_lossy().into_owned();
    manager
}

fn put(path: &Path, content: &str) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, content).expect("write");
}

fn set_packages(settings: &mut SettingsManager, scope: SettingsScope, packages: Value) {
    let mut values = Settings::new(); values.insert("packages".into(), packages);
    settings.set(scope, &values).expect("settings");
}

#[tokio::test]
async fn fake_runner_models_install_and_uninstall_storage() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = FakeRunner::default();
    let root = dir.path().to_string_lossy().into_owned();
    let command = PackageCommand { program: "npm".into(), args: vec!["install".into(), "demo@1.0.0".into(), "--prefix".into(), root.clone()], cwd: None, capture: false, timeout_ms: None, env: Default::default() };
    // When
    runner.run(command).await.expect("install");
    // Then
    assert!(Path::new(&root).join("node_modules/demo/package.json").exists());
}

#[tokio::test]
async fn npm_install_persists_in_callers_settings_and_emits_progress() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    let runner = Arc::new(FakeRunner::default()); let events = Arc::new(Mutex::new(Vec::new()));
    let mut manager = manager(&dir, &mut settings, runner.clone());
    let sink = events.clone(); manager.set_progress_callback(Some(Arc::new(move |event| sink.lock().expect("events").push(event))));
    // When
    manager.install_and_persist("npm:@scope/demo@1.0.0", None).await.expect("install");
    // Then
    assert_eq!(manager.settings_manager.get_global()["packages"], json!(["npm:@scope/demo@1.0.0"]));
    assert!(manager.get_installed_path("npm:@scope/demo", InstalledSourceScope::User).expect("path").is_some());
    let events = events.lock().expect("events");
    assert_eq!(events.iter().map(|event| event.event_type).collect::<Vec<_>>(), vec![ProgressEventType::Start, ProgressEventType::Complete]);
    assert_eq!(runner.calls()[0].args.last().expect("arg"), "--legacy-peer-deps");
}

#[tokio::test]
async fn failed_install_does_not_persist_and_emits_error() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    let runner = Arc::new(FakeRunner { fail: true, ..Default::default() });
    let mut manager = manager(&dir, &mut settings, runner); let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone(); manager.set_progress_callback(Some(Arc::new(move |event| sink.lock().expect("events").push(event))));
    // When
    let result = manager.install_and_persist("npm:demo", None).await;
    // Then
    assert!(result.is_err()); assert!(manager.packages(SourceScope::User).is_empty());
    assert_eq!(events.lock().expect("events").last().expect("event").event_type, ProgressEventType::Error);
}

#[tokio::test]
async fn remove_and_persist_uninstalls_and_removes_identity() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!([{"source":"npm:demo@1.0.0","prompts":[]}]));
    put(&dir.path().join("agent/npm/node_modules/demo/package.json"), "{}");
    let runner = Arc::new(FakeRunner::default()); let mut manager = manager(&dir, &mut settings, runner);
    // When
    let changed = manager.remove_and_persist("npm:demo", None).await.expect("remove");
    // Then
    assert!(changed); assert!(manager.packages(SourceScope::User).is_empty());
    assert!(!dir.path().join("agent/npm/node_modules/demo").exists());
}

#[test]
fn replacing_source_keeps_filtered_record_and_normalizes_local_paths() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!([{"source":"npm:demo@1.0.0","prompts":["keep.md"],"autoload":false}]));
    let mut manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let changed = manager.add_source_to_settings("npm:demo@2.0.0", None).expect("persist");
    // Then
    assert!(changed); assert_eq!(manager.packages(SourceScope::User), vec![json!({"source":"npm:demo@2.0.0","prompts":["keep.md"],"autoload":false})]);
}

#[test]
fn local_source_is_persisted_relative_to_project_configuration() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    let mut manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    manager.add_source_to_settings("./resources", Some(PackageOptions { local: true })).expect("persist");
    // Then
    assert_eq!(manager.packages(SourceScope::Project), vec![json!("../resources")]);
}

#[tokio::test]
async fn untrusted_project_install_never_executes_runner() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(false);
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    // When
    let result = manager.install("npm:demo", Some(PackageOptions { local: true })).await;
    // Then
    assert!(result.is_err()); assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn missing_source_skip_does_not_install() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["git:github.com/user/repo"]));
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    let callback: &MissingSourceCallback = &|_| Box::pin(async { MissingSourceAction::Skip });
    // When
    let paths = manager.resolve(Some(callback)).await.expect("resolve");
    // Then
    assert!(paths.prompts.is_empty()); assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn missing_source_error_does_not_install() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["git:github.com/user/repo"]));
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    let callback: &MissingSourceCallback = &|_| Box::pin(async { MissingSourceAction::Error });
    // When
    let result = manager.resolve(Some(callback)).await;
    // Then
    assert!(result.is_err()); assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn missing_source_install_discovers_real_fake_installed_resources() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["npm:demo@1.0.0"]));
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    let callback: &MissingSourceCallback = &|_| Box::pin(async { MissingSourceAction::Install });
    // When
    let paths = manager.resolve(Some(callback)).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert!(paths.prompts[0].path.ends_with("prompts/npm.md"));
    assert!(runner.calls().iter().any(|command| command.args.first().is_some_and(|arg| arg == "install")));
}

#[tokio::test]
async fn offline_resolution_does_not_invoke_missing_callback() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["git:github.com/user/repo"]));
    let runner = Arc::new(FakeRunner::default()); let mut manager = manager(&dir, &mut settings, runner.clone());
    for key in ["MAHO_OFFLINE", "PI_OFFLINE", "SENPI_OFFLINE", "OMO_OFFLINE"] { manager.env.insert(key.into(), "yes".into()); }
    let callback: &MissingSourceCallback = &|_| panic!("offline callback");
    // When
    let paths = manager.resolve(Some(callback)).await.expect("resolve");
    // Then
    assert!(paths.prompts.is_empty()); assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn package_filter_empty_array_keeps_disabled_resources() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("pkg/prompts/a.md"), "a");
    set_packages(&mut settings, SettingsScope::Global, json!([{"source":"../pkg","prompts":[]}]));
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert!(!paths.prompts[0].enabled);
}

#[tokio::test]
async fn project_autoload_delta_uses_user_checkout_and_preserves_other_resources() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("agent/npm/node_modules/demo/package.json"), r#"{"version":"1.0.0"}"#);
    put(&dir.path().join("agent/npm/node_modules/demo/prompts/a.md"), "a");
    put(&dir.path().join("agent/npm/node_modules/demo/prompts/b.md"), "b");
    set_packages(&mut settings, SettingsScope::Global, json!(["npm:demo@1.0.0"]));
    set_packages(&mut settings, SettingsScope::Project, json!([{"source":"npm:demo@2.0.0","autoload":false,"prompts":["-prompts/a.md"]}]));
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 2);
    assert!(!paths.prompts.iter().find(|entry| entry.path.ends_with("a.md")).expect("a").enabled);
    assert!(paths.prompts.iter().find(|entry| entry.path.ends_with("b.md")).expect("b").enabled);
    assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn temporary_manifest_system_scope_applies_only_to_temporary_sources() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("pkg/package.json"), r#"{"pi":{"system":true,"prompts":["prompts/*.md"]}}"#);
    put(&dir.path().join("pkg/prompts/a.md"), "a");
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve_extension_sources(&["./pkg".into()], Some(ResolveSourcesOptions { temporary: true, local: false })).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts[0].metadata.scope, SourceScope::System); assert!(paths.extensions.is_empty());
}

#[tokio::test]
async fn git_clone_uses_ref_and_remove_cleans_owned_checkout() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    // When
    manager.install("git:git@github.com:user/repo@v1", None).await.expect("install");
    // Then
    assert_eq!(runner.calls()[0].args[1], "git@github.com:user/repo");
    assert_eq!(runner.calls()[1].args, vec!["checkout", "v1"]);
    assert!(dir.path().join("agent/git/github.com/user/repo/prompts/git.md").exists());
}

#[tokio::test]
async fn git_remove_cleans_update_marker_and_empty_parents() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    let target = dir.path().join("agent/git/github.com/user/repo");
    put(&target.join("prompts/a.md"), "a");
    put(&target.parent().expect("parent").join(".repo.pi-update-incomplete"), "");
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    manager.remove("git:github.com/user/repo", None).await.expect("remove");
    // Then
    assert!(!target.exists()); assert!(!target.parent().expect("parent").exists());
}

#[tokio::test]
async fn pinned_git_update_reconciles_ref_but_same_head_avoids_reset() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("agent/git/github.com/user/repo/prompts/a.md"), "a");
    set_packages(&mut settings, SettingsScope::Global, json!(["git:github.com/user/repo@v2"]));
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    // When
    manager.update(None).await.expect("update");
    // Then
    assert_eq!(runner.calls()[0].args, vec!["fetch", "origin", "v2"]);
    assert!(!runner.calls().iter().any(|command| command.args.first().is_some_and(|arg| arg == "reset")));
}

#[tokio::test]
async fn update_batches_unpinned_npm_and_skips_exact_versions() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["npm:a", "npm:b@^2.0.0", "npm:fixed@1.0.0"]));
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    // When
    manager.update(None).await.expect("update");
    // Then
    let calls = runner.calls(); assert_eq!(calls.len(), 1);
    assert_eq!(&calls[0].args[..3], &["install", "a@latest", "b@^2.0.0"]);
}

#[tokio::test]
async fn update_keeps_installed_newer_version() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["npm:demo"]));
    put(&dir.path().join("agent/npm/node_modules/demo/package.json"), r#"{"version":"3.0.0"}"#);
    let runner = Arc::new(FakeRunner::default()); let manager = manager(&dir, &mut settings, runner.clone());
    // When
    manager.update(None).await.expect("update");
    // Then
    assert_eq!(runner.calls().len(), 1); assert_eq!(runner.calls()[0].args[0], "view");
    assert_eq!(runner.calls()[0].timeout_ms, Some(10_000));
}

#[test]
fn pattern_force_excludes_win_and_skill_directory_matches() {
    // Given
    let path = "/pkg/skills/demo/SKILL.md";
    // When
    let enabled = enabled_by_patterns(path, &["!**".into(), "+skills/demo".into(), "-skills/demo".into()], "/pkg");
    // Then
    assert!(!enabled); assert!(pattern_matches(path, "demo", "/pkg", false));
    assert!(!pattern_matches(path, "demo", "/pkg", true));
}

#[test]
fn npm_ranges_preserve_npm_minor_and_exact_behavior() {
    // Given
    let cases = [("1.9.0", "1", true), ("2.0.0", "1", false), ("1.3.0", "1.2", false), ("1.2.9", "1.2", true), ("1.3.0", "^1.2.0", true), ("2.0.0", ">=1 <2", false), ("1.2.0", "latest", true)];
    // When / Then
    for (version, range, expected) in cases { assert_eq!(npm_matches(version, Some(range)), expected, "{version} {range}"); }
}

#[test]
fn git_sources_normalize_identity_and_reject_traversal() {
    // Given
    let sources = ["git:git@github.com:user/repo@v1", "https://github.com/user/repo.git", "ssh://git@github.com/user/repo"];
    // When / Then
    for source in sources {
        let ParsedSource::Git { host, path, .. } = parse_source(source) else { panic!("git source"); };
        assert_eq!(host, "github.com"); assert_eq!(path, "user/repo");
    }
    assert!(matches!(parse_source("github.com/user/repo"), ParsedSource::Local(_)));
    assert!(matches!(parse_source("git:github.com/user/%2e%2e/repo"), ParsedSource::Local(_)));
}

#[test]
fn npm_wrappers_select_package_manager_specific_peer_flags() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir");
    for (command, flags) in [
        (json!(["bun"]), vec!["--cwd", "--omit=peer"]),
        (json!(["pnpm"]), vec!["--prefix", "--config.auto-install-peers=false", "--config.strict-dep-builds=false"]),
        (json!(["wrapper", "--", "npm"]), vec!["--prefix", "--legacy-peer-deps"]),
    ] {
        let mut settings = settings(true); let mut values = Settings::new(); values.insert("npmCommand".into(), command);
        settings.set(SettingsScope::Global, &values).expect("settings");
        let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
        // When
        let args = manager.npm_install_args(vec!["demo".into()], "/managed").expect("args");
        // Then
        for flag in flags { assert!(args.iter().any(|arg| arg == flag)); }
    }
}

#[derive(Default)]
struct ConcurrentGitRunner {
    arrived: std::sync::atomic::AtomicUsize,
    barrier: tokio::sync::Notify,
}

#[async_trait::async_trait]
impl PackageProcessRunner for ConcurrentGitRunner {
    async fn run(&self, command: PackageCommand) -> PackageResult<String> {
        if command.args.first().is_some_and(|arg| arg == "clone") {
            let notification = self.barrier.notified();
            tokio::pin!(notification);
            notification.as_mut().enable();
            if self.arrived.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == 4 {
                self.barrier.notify_waiters();
            } else {
                tokio::time::timeout(std::time::Duration::from_secs(1), notification).await
                    .map_err(|_| PackageManagerError::Message("git updates did not overlap".into()))?;
            }
            std::fs::create_dir_all(command.args.last().expect("target"))?;
        }
        Ok(String::new())
    }
    fn run_sync(&self, _: PackageCommand) -> PackageResult<String> { Ok(String::new()) }
}

#[tokio::test]
async fn git_updates_start_four_operations_before_waiting_for_completion() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["git:github.com/u/a", "git:github.com/u/b", "git:github.com/u/c", "git:github.com/u/d"]));
    let runner = Arc::new(ConcurrentGitRunner::default());
    let mut manager = DefaultPackageManager::with_runner(PackageManagerOptions {
        cwd: &dir.path().to_string_lossy(), agent_dir: &dir.path().join("agent").to_string_lossy(), settings_manager: &mut settings,
    }, runner.clone());
    manager.env.clear();
    // When
    manager.update(None).await.expect("update");
    // Then
    assert_eq!(runner.arrived.load(std::sync::atomic::Ordering::SeqCst), 4);
}

#[tokio::test]
async fn manifest_overrides_cannot_reenable_manifest_excluded_paths() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("pkg/package.json"), r#"{"pi":{"prompts":["prompts/*.md","!prompts/drop.md"]}}"#);
    put(&dir.path().join("pkg/prompts/keep.md"), "keep");
    put(&dir.path().join("pkg/prompts/drop.md"), "drop");
    set_packages(&mut settings, SettingsScope::Global, json!([{"source":"../pkg","prompts":["+prompts/drop.md","+prompts/keep.md"]}]));
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert!(paths.prompts[0].path.ends_with("keep.md"));
}

#[tokio::test]
async fn installed_manifest_cannot_upgrade_user_scope_to_system() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("pkg/package.json"), r#"{"pi":{"system":true,"themes":["themes/a.json"]}}"#);
    put(&dir.path().join("pkg/themes/a.json"), "{}");
    set_packages(&mut settings, SettingsScope::Global, json!(["../pkg"]));
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.themes[0].metadata.scope, SourceScope::User);
}

#[tokio::test]
async fn untrusted_project_skips_current_config_but_keeps_user_discovery() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(false);
    put(&dir.path().join(crate::config::config_dir_name()).join("prompts/project.md"), "project");
    put(&dir.path().join("agent/prompts/user.md"), "user");
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert_eq!(paths.prompts[0].metadata.scope, SourceScope::User);
}

#[tokio::test]
async fn autoload_delta_changes_only_explicitly_matched_resources() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("pkg/prompts/a.md"), "a"); put(&dir.path().join("pkg/prompts/b.md"), "b");
    set_packages(&mut settings, SettingsScope::Global, json!([{"source":"../pkg","autoload":false,"prompts":["+prompts/b.md"]}]));
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert!(paths.prompts[0].enabled); assert!(paths.prompts[0].path.ends_with("b.md"));
}

#[cfg(unix)]
#[tokio::test]
async fn canonical_deduplication_keeps_higher_precedence_resource() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    let real = dir.path().join("agent/prompts/a.md"); put(&real, "a");
    let project_dir = dir.path().join(crate::config::config_dir_name()).join("prompts");
    std::fs::create_dir_all(&project_dir).expect("mkdir");
    std::os::unix::fs::symlink(&real, project_dir.join("alias.md")).expect("symlink");
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert_eq!(paths.prompts[0].metadata.scope, SourceScope::Project);
}

#[tokio::test]
async fn legacy_global_npm_resources_resolve_without_managed_install() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    let root = dir.path().join("legacy");
    put(&root.join("demo/package.json"), r#"{"version":"1.0.0"}"#);
    put(&root.join("demo/prompts/a.md"), "a");
    set_packages(&mut settings, SettingsScope::Global, json!(["npm:demo@1.0.0"]));
    let runner = Arc::new(FakeRunner { global_root: Some(root.to_string_lossy().into_owned()), ..Default::default() });
    let manager = manager(&dir, &mut settings, runner.clone());
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert_eq!(runner.calls()[0].args, vec!["root", "-g"]);
    assert!(!runner.calls().iter().any(|command| command.args[0] == "install"));
}

#[tokio::test]
async fn explicit_resources_precede_auto_resources_and_native_extensions_stay_empty() {
    // Given
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    put(&dir.path().join("agent/prompts/a.md"), "a");
    put(&dir.path().join("agent/extensions/a.ts"), "throw new Error()");
    let mut values = Settings::new(); values.insert("prompts".into(), json!(["prompts/a.md", "-prompts/a.md"]));
    settings.set(SettingsScope::Global, &values).expect("settings");
    let manager = manager(&dir, &mut settings, Arc::new(FakeRunner::default()));
    // When
    let paths = manager.resolve(None).await.expect("resolve");
    // Then
    assert_eq!(paths.prompts.len(), 1); assert!(!paths.prompts[0].enabled);
    assert_eq!(paths.prompts[0].metadata.source, "local"); assert!(paths.extensions.is_empty());
}

#[test]
fn npm_partial_comparators_and_prerelease_admission_follow_npm() {
    let cases = [
        ("1.9.0", ">1", false), ("2.0.0", ">1", true),
        ("1.2.9", "<=1.2", true), ("1.3.0", "<=1.2", false),
        ("2.0.0-alpha", "1 - 2", false), ("2.9.0", "1 - 2", true),
        ("1.2.3-alpha.2", ">=1.2.3-alpha.1 <2", true),
        ("1.3.0-alpha", ">=1.2.3-alpha.1 <2", false),
        ("1.2.3+two", "1.2.3+one", true),
        ("1.2.3-alpha.2", "1.2.3-alpha.1", false),
        ("1.2.4", "1.2.3 - 1.2.5", true),
        ("1.2.3-alpha", "*", false), ("1.2.3-alpha", "", true),
        ("1.2.3-alpha", "latest", true), ("1.0.0", ">*", false),
        ("1.2.9", "=1.2", true), ("1.3.0", "=1.2", false),
        ("1.2.9", ">=\t 1.2 <\t2", true),
    ];
    for (version, range, expected) in cases { assert_eq!(npm_matches(version, Some(range)), expected, "{version} {range}"); }
    assert!(parse_version("=1.2.3").is_none());
    assert!(parse_version("v1.2.3").is_some());
}

#[tokio::test]
async fn npm_single_view_version_is_not_range_filtered_or_build_ordered() {
    let dir = tempfile::tempdir().expect("tempdir"); let mut settings = settings(true);
    set_packages(&mut settings, SettingsScope::Global, json!(["npm:pkg@^1"]));
    put(&dir.path().join("agent/npm/node_modules/pkg/package.json"), r#"{"version":"1.0.0"}"#);
    let runner = Arc::new(FakeRunner { latest: Some("\"2.0.0\"".into()), ..Default::default() });
    let manager = manager(&dir, &mut settings, runner.clone());
    manager.update(None).await.expect("update");
    assert!(runner.calls().iter().any(|command| command.args.first().is_some_and(|arg| arg == "install")));
    assert!(parse_version("1.0.0+a").expect("version").cmp_precedence(&parse_version("1.0.0+z").expect("version")).is_eq());
}

#[test]
fn nested_extglobs_and_hidden_segments_match_minimatch_filters() {
    let cases = [
        ("prompts/a.md", "prompts/@(a|b).md", true),
        ("prompts/c.md", "prompts/@(a|b).md", false),
        ("ab.md", "+(a|b).md", true), ("c.md", "*(a|b)c.md", true),
        ("c.md", "?(a|b).md", false), ("ab.md", "@(a?(b)|c).md", true),
        ("a.txt", "*.!(md|json)", true), ("a.md", "*.!(md|json)", false),
        ("a.md", "@({a,b}|c).md", true),
        ("x/.hidden/a.md", "**/.hidden/*.md", true),
        (".hidden/x/a.md", ".hidden/**/*.md", true),
        ("x/.other/a.md", "**/.hidden/*.md", false),
    ];
    for (path, pattern, expected) in cases { assert_eq!(minimatch_path(path, pattern), expected, "{path} {pattern}"); }
}

#[test]
fn hosted_shorthand_keeps_pinned_clone_url_and_storage_identity() {
    for (source, host, path, repo) in [
        ("git:user/repo@v1", "github.com", "user/repo", "https://user/repo"),
        ("git:github:user/repo#v1", "github.com", "user/repo", "https://github:user/repo#v1"),
        ("git:gitlab:group/sub/repo#v1", "gitlab.com", "group/sub/repo", "https://gitlab:group/sub/repo#v1"),
        ("git:bitbucket:user/repo#v1", "bitbucket.org", "user/repo", "https://bitbucket:user/repo#v1"),
        ("git:gist:abc#v1", "gist.github.com", "null/abc", "https://gist:abc#v1"),
        ("git:sourcehut:~user/repo#v1", "git.sr.ht", "~user/repo", "https://sourcehut:~user/repo#v1"),
    ] {
        let Some(ParsedSource::Git { host: actual_host, path: actual_path, repo: actual_repo, reference }) = parse_git_source(source) else { panic!("git {source}"); };
        assert_eq!(actual_host, host); assert_eq!(actual_path, path); assert_eq!(actual_repo, repo); assert_eq!(reference.as_deref(), Some("v1"));
    }
    assert!(parse_git_source("user/repo").is_none());
    let Some(ParsedSource::Git { path, reference, .. }) = parse_git_source("https://github.com/user/repo/tree/main") else { panic!("tree URL"); };
    assert_eq!(path, "user/repo"); assert_eq!(reference.as_deref(), Some("main"));
}

#[cfg(target_os = "linux")]
#[test]
fn live_stdout_takeover_duplicates_stderr_descriptor_without_buffering() {
    use std::os::fd::{AsRawFd, OwnedFd};
    let redirected = OwnedFd::try_from(package_stdout(false, true).expect("stdio")).expect("owned descriptor");
    assert_eq!(std::fs::read_link(format!("/proc/self/fd/{}", redirected.as_raw_fd())).expect("redirected"), std::fs::read_link("/proc/self/fd/2").expect("stderr"));
    assert!(OwnedFd::try_from(package_stdout(false, false).expect("stdio")).is_err());
    assert!(OwnedFd::try_from(package_stdout(true, true).expect("stdio")).is_err());
}
