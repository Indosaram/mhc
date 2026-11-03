//! `/ast-grep` command of `index.ts` (pi-ast-grep 4a7d1bee): the status report for `''` and the
//! `install` path, with `PI_OFFLINE`, `PATH` and the cache variables injected through the
//! environment seam instead of the process environment.
use maho_ext_api::*;
use maho_ext_pi_ast_grep::{AstGrep, downloader::EnvReader};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, PoisonError},
};

const SG: &str = if cfg!(windows) { "sg.exe" } else { "sg" };
const FAILED: &str =
    "Auto-download failed. Try: npm install -g @ast-grep/cli or brew install ast-grep";

#[derive(Debug, PartialEq)]
enum UiEvent {
    Status(String, Option<String>),
    Notify(String, NotificationType),
}

#[derive(Default)]
struct RecordingUi(Mutex<Vec<UiEvent>>);
impl RecordingUi {
    fn push(&self, event: UiEvent) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(event);
    }
    fn take(&self) -> Vec<UiEvent> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }
}
impl ExtensionUi for RecordingUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: ExtensionUiDialogOptions,
    ) -> UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.push(UiEvent::Notify(message.into(), kind));
    }
    fn set_status(&self, key: &str, text: Option<&str>) {
        self.push(UiEvent::Status(key.into(), text.map(str::to_owned)));
    }
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> {
        Box::pin(async { Err("headless fixture".into()) })
    }
    fn theme(&self) -> Theme {
        Theme::default()
    }
}

struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str {
        "fixture"
    }
    fn session_file(&self) -> Option<&Path> {
        None
    }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_branch(&self) -> Vec<SessionEntry> {
        Vec::new()
    }
    fn get_leaf_id(&self) -> Option<String> {
        None
    }
    fn get_session_name(&self) -> Option<String> {
        None
    }
}

struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> {
        Vec::new()
    }
    fn get_available(&self) -> Vec<Model> {
        Vec::new()
    }
    fn find(&self, _: &str, _: &str) -> Option<Model> {
        None
    }
    fn has_configured_auth(&self, _: &Model) -> bool {
        false
    }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> {
        Box::pin(async { Ok(None) })
    }
}

fn context(cwd: &Path, ui: Arc<RecordingUi>) -> ExtensionContext {
    ExtensionContext {
        ui,
        mode: ExtensionMode::Print,
        has_ui: false,
        cwd: cwd.into(),
        agent_dir: cwd.join("agent"),
        session_manager: Arc::new(Session),
        model_registry: Arc::new(Registry),
        model: None,
        thinking_level: None,
        service_tier: None,
        effective_service_tier: None,
        scoped_models: Vec::new(),
        goal_store_file: None,
        loaded_extension_paths: Vec::new(),
        signal: None,
        steering_signal: None,
        is_idle_fn: Arc::new(|| true),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(),
        update_tool_hook_status: None,
        idle_coordinator: None,
        logger: None,
        defer_macrotask: None,
        compaction_signal: Default::default(),
    }
}

/// A fixture root with an isolated home, cache and `PATH`; nothing is read from the process env.
struct Fixture {
    root: tempfile::TempDir,
    cache: PathBuf,
    path_sg: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = root.path().join("cache").join("pi-ast-grep").join("bin");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        let path_sg = bin.join(SG);
        // `findSgCliPathSync` only accepts binaries larger than 10 000 bytes.
        std::fs::write(&path_sg, vec![0_u8; 10_001]).expect("PATH sg");
        Self {
            root,
            cache,
            path_sg,
        }
    }
    fn seed_cache(&self) -> PathBuf {
        std::fs::create_dir_all(&self.cache).expect("cache dir");
        let cached = self.cache.join(SG);
        std::fs::write(&cached, vec![0_u8; 10_001]).expect("cached sg");
        cached
    }
    fn env(&self, offline: Option<&str>) -> EnvReader {
        let root = self.root.path();
        let mut vars: HashMap<&'static str, OsString> = HashMap::from([
            ("HOME", root.join("home").into_os_string()),
            ("USERPROFILE", root.join("home").into_os_string()),
            ("XDG_CACHE_HOME", root.join("cache").into_os_string()),
            ("LOCALAPPDATA", root.join("cache").into_os_string()),
            ("PATH", root.join("bin").into_os_string()),
        ]);
        if let Some(offline) = offline {
            vars.insert("PI_OFFLINE", offline.into());
        }
        Arc::new(move |key: &str| vars.get(key).cloned())
    }
    async fn run(&self, env: EnvReader, args: &str) -> Vec<UiEvent> {
        let root = self.root.path();
        let mut api = ExtensionApi::new(
            LoadedExtension::new("pi-ast-grep", root.into(), SourceInfo::default()),
            ExtensionSessionProfile::default(),
            EventBus::default(),
            ExtensionRuntime::default(),
        );
        AstGrep {
            package_entry: Some(root.join("package/extension.js")),
        }
        .register_with_env(&mut api, env);
        let ui = Arc::new(RecordingUi::default());
        let ctx = context(root, Arc::clone(&ui));
        let command = api
            .registered
            .commands
            .iter()
            .find(|command| command.name == "ast-grep")
            .expect("ast-grep command registered");
        (command.handler)(args, &ctx).await.expect("command");
        ui.take()
    }
}

fn status_report(cache: &Path, cached: Option<&Path>, local: Option<&Path>) -> String {
    format!(
        "pi-ast-grep\n  Cache dir : {}\n  Cached sg : {}\n  Local sg  : {}",
        cache.display(),
        cached.map_or_else(
            || "not downloaded".into(),
            |path| path.display().to_string()
        ),
        local.map_or_else(|| "not on PATH".into(), |path| path.display().to_string()),
    )
}

fn install_events(message: String, kind: NotificationType) -> Vec<UiEvent> {
    vec![
        UiEvent::Status(
            "pi-ast-grep".into(),
            Some("Downloading sg binary...".into()),
        ),
        UiEvent::Status("pi-ast-grep".into(), None),
        UiEvent::Notify(message, kind),
    ]
}

#[tokio::test]
async fn empty_args_report_the_injected_cache_dir_and_path_binary() {
    let fixture = Fixture::new();
    let events = fixture.run(fixture.env(Some("1")), "").await;
    assert_eq!(
        events,
        vec![UiEvent::Notify(
            status_report(&fixture.cache, None, Some(&fixture.path_sg)),
            NotificationType::Info,
        )]
    );

    let cached = fixture.seed_cache();
    let events = fixture.run(fixture.env(Some("1")), "  ").await;
    assert_eq!(
        events,
        vec![UiEvent::Notify(
            status_report(&fixture.cache, Some(&cached), Some(&cached)),
            NotificationType::Info,
        )]
    );
}

#[tokio::test]
async fn install_with_offline_env_fails_even_when_a_binary_is_cached() {
    let fixture = Fixture::new();
    fixture.seed_cache();
    for offline in ["1", "true"] {
        let events = fixture.run(fixture.env(Some(offline)), "install").await;
        assert_eq!(
            events,
            install_events(FAILED.into(), NotificationType::Error),
            "PI_OFFLINE={offline}"
        );
    }
}

#[tokio::test]
async fn install_without_offline_env_returns_the_cached_binary() {
    let fixture = Fixture::new();
    let cached = fixture.seed_cache();
    for offline in [None, Some("0")] {
        let events = fixture.run(fixture.env(offline), " install ").await;
        assert_eq!(
            events,
            install_events(
                format!("ast-grep ready: {}", cached.display()),
                NotificationType::Info
            ),
            "PI_OFFLINE={offline:?}"
        );
    }
}
