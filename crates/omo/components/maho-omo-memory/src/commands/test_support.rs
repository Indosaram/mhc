//! Test harness for the memory command suite: fake contexts, temp identities,
//! seeded git repos, and overridable deps.
//! Port of `components/memory/commands/commands.test-support.ts` at pin 77f3067f1.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, PoisonError,
    },
};

use maho_ext_api::{
    CustomMessage, ExtensionActions, ExtensionFailure, JsonValue, SendMessageOptions,
    SendUserMessageOptions, ToolInfo, UiFuture, UserMessageContent,
};
use memory_core::{
    git::{
        GitError, GitHookInstaller, GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile,
        InitializeGitRepoOptions,
    },
    identity::layout::build_identity_paths,
    memfs::install_hooks,
    reflection::ReflectionEvent,
};

use crate::binding::MemorySessionBinding;
use crate::context::MemoryIdentityContext;
use crate::facts_wiring::FactsExtractorWork;

use super::people_ask::{PeopleAskRequest, PeopleAskRunner};
use super::types::{
    CommandContext, DreamCommandOutcome, DreamRequestSink, ManualDreamCommandRequest,
    ManualReflectionRequest, MemoryCommandDeps, MemoryCommandIdentity, MemoryCommandUi,
    NotifyLevel,
};

pub const TEST_IDENTITY: &str = "test-identity";

pub struct FakeCommandUi {
    pub notifications: Mutex<Vec<(String, NotifyLevel)>>,
    pub confirms: Mutex<Vec<(String, String)>>,
    pub selects: Mutex<Vec<(String, Vec<String>)>>,
    confirm_result: AtomicBool,
    select_result: Mutex<Option<String>>,
}

impl FakeCommandUi {
    pub fn new(confirm_result: bool) -> Self {
        Self {
            notifications: Mutex::new(Vec::new()),
            confirms: Mutex::new(Vec::new()),
            selects: Mutex::new(Vec::new()),
            confirm_result: AtomicBool::new(confirm_result),
            select_result: Mutex::new(None),
        }
    }

    pub fn set_confirm_result(&self, value: bool) {
        self.confirm_result.store(value, Ordering::SeqCst);
    }

    pub fn set_select_result(&self, value: Option<String>) {
        *self.select_result.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }

    pub fn notifications(&self) -> Vec<(String, NotifyLevel)> {
        self.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn last_level(&self) -> Option<NotifyLevel> {
        self.notifications()
            .last()
            .map(|(_, level)| *level)
    }
}

impl MemoryCommandUi for FakeCommandUi {
    fn notify(&self, message: &str, level: NotifyLevel) {
        self.notifications
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((message.to_owned(), level));
    }

    fn supports_confirm(&self) -> bool {
        true
    }

    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> UiFuture<'a, bool> {
        self.confirms
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((title.to_owned(), message.to_owned()));
        let result = self.confirm_result.load(Ordering::SeqCst);
        Box::pin(async move { result })
    }

    fn select<'a>(&'a self, title: &'a str, options: &'a [String]) -> UiFuture<'a, Option<String>> {
        self.selects
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((title.to_owned(), options.to_vec()));
        let result = self
            .select_result
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Box::pin(async move { result })
    }
}

pub struct FakeActions {
    pub user_messages: Mutex<Vec<UserMessageContent>>,
    pub order: Arc<Mutex<Vec<String>>>,
}

impl FakeActions {
    pub fn new(order: Arc<Mutex<Vec<String>>>) -> Self {
        Self { user_messages: Mutex::new(Vec::new()), order }
    }

    pub fn user_messages(&self) -> Vec<UserMessageContent> {
        self.user_messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ExtensionActions for FakeActions {
    fn send_message(
        &self,
        _message: CustomMessage,
        _options: SendMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        Ok(())
    }

    fn send_user_message(
        &self,
        content: UserMessageContent,
        _options: SendUserMessageOptions,
    ) -> Result<(), ExtensionFailure> {
        self.order
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("sendUserMessage".to_owned());
        self.user_messages
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(content);
        Ok(())
    }

    fn append_entry(&self, _custom_type: &str, _data: Option<JsonValue>) -> Result<(), ExtensionFailure> {
        Ok(())
    }

    fn get_all_tools(&self) -> Result<Vec<ToolInfo>, ExtensionFailure> {
        Ok(Vec::new())
    }
}

#[derive(Default)]
pub struct FakeContextOptions {
    pub session_id: Option<String>,
    pub has_ui: Option<bool>,
    pub agent_dir: Option<PathBuf>,
    pub with_wait_for_idle: Option<bool>,
}

pub struct FakeContext {
    pub ui: Arc<FakeCommandUi>,
    pub order: Arc<Mutex<Vec<String>>>,
    pub ctx: CommandContext,
}

pub fn fake_command_context(options: FakeContextOptions) -> FakeContext {
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let has_ui = options.has_ui.unwrap_or(true);
    let ui = Arc::new(FakeCommandUi::new(true));
    let wait_order = order.clone();
    let wait_for_idle: Option<Arc<dyn Fn() -> UiFuture<'static, ()> + Send + Sync>> =
        match options.with_wait_for_idle.unwrap_or(true) {
            true => Some(Arc::new(move || {
                let order = wait_order.clone();
                Box::pin(async move {
                    order
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push("waitForIdle".to_owned());
                })
            })),
            false => None,
        };
    let ctx = CommandContext {
        ui: ui.clone(),
        has_ui,
        cwd: PathBuf::from("/project"),
        agent_dir: options.agent_dir.unwrap_or_default(),
        session_id: Some(options.session_id.unwrap_or_else(|| "session-1".to_owned())),
        model_registry: None,
        wait_for_idle,
    };
    FakeContext { ui, order, ctx }
}

pub fn temp_identity() -> (tempfile::TempDir, MemoryCommandIdentity) {
    let root = tempfile::tempdir().expect("temp identity root");
    let identity_paths = build_identity_paths(root.path(), TEST_IDENTITY);
    (
        root,
        MemoryCommandIdentity { identity: TEST_IDENTITY.to_owned(), identity_paths },
    )
}

pub fn identity_context(identity: &MemoryCommandIdentity) -> MemoryIdentityContext {
    MemoryIdentityContext::new(
        identity.identity.clone(),
        identity.identity_paths.clone(),
        MemorySessionBinding {
            identity: identity.identity.clone(),
            repo_path_hash: "hash".to_owned(),
            bound_at: 1.0,
        },
    )
}

pub fn seed(relative_path: &str, content: &str) -> GitSeedFile {
    GitSeedFile { relative_path: relative_path.to_owned(), content: content.to_owned() }
}

pub fn hook_installer() -> GitHookInstaller {
    Arc::new(|dir: &std::path::Path| install_hooks(dir).map(|_| ()).map_err(GitError::Io))
}

pub fn open_repo(identity: &MemoryCommandIdentity) -> GitMemoryRepo {
    GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: identity.identity_paths.repo.clone(),
        agent_id: identity.identity.clone(),
        exec: None,
        install_hooks: Some(hook_installer()),
    })
    .expect("repo")
}

pub fn seeded_repo(identity: &MemoryCommandIdentity, seed_files: Vec<GitSeedFile>) -> GitMemoryRepo {
    let repo = open_repo(identity);
    repo.init(Some(InitializeGitRepoOptions {
        author_name: None,
        seed_files,
        install_hooks: Some(hook_installer()),
    }))
    .expect("init");
    repo
}

/// Mirrors the upstream `memorySettings()` fixture.
pub fn memory_settings() -> serde_json::Value {
    serde_json::json!({
        "enabled": true,
        "agent": "auto",
        "tool_exposure": "direct",
        "reflection": {
            "enabled": true,
            "trigger": { "step_count": 25, "on_compaction": true },
            "merge": "auto",
            "category": "quick",
            "timeout_minutes": 15,
            "sandbox": "auto"
        },
        "nudge": { "enabled": true, "every_user_turns": 10 },
        "facts": { "enabled": true, "debounce_settles": 4 },
        "dream": {
            "enabled": true,
            "idle_minutes": 30,
            "min_hours_between": 24,
            "shutdown_launch": true,
            "auto_select_max": 5,
            "auto_select_max_chars": 150000
        },
        "people": { "enabled": true, "max_entries": 40, "max_entry_chars": 200 },
        "soul": { "edit_notice": true },
        "sync": { "enabled": true },
        "search": { "enabled": true },
        "compile_warn_tokens": 30000,
        "agents": {}
    })
}

struct FakeDreamSink {
    requests: Arc<Mutex<Vec<ManualDreamCommandRequest>>>,
    outcome: Arc<Mutex<DreamCommandOutcome>>,
}

impl DreamRequestSink for FakeDreamSink {
    fn request(
        &self,
        request: ManualDreamCommandRequest,
    ) -> super::types::BoxFuture<'_, Result<DreamCommandOutcome, String>> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
        let outcome = self
            .outcome
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        Box::pin(async move { Ok(outcome) })
    }
}

pub struct FakeDeps {
    pub deps: MemoryCommandDeps,
    pub busts: Arc<Mutex<Vec<String>>>,
    pub reflection_requests: Arc<Mutex<Vec<ManualReflectionRequest>>>,
    pub dream_requests: Arc<Mutex<Vec<ManualDreamCommandRequest>>>,
    pub receipt: Arc<Mutex<(String, String)>>,
    pub alive_pids: Arc<Mutex<BTreeSet<u32>>>,
    pub retry_triggers: Arc<Mutex<Vec<String>>>,
    pub dream_outcome: Arc<Mutex<DreamCommandOutcome>>,
    pub people_asks: Arc<Mutex<Vec<PeopleAskRequest>>>,
    pub actions: Arc<FakeActions>,
}

pub struct FakeDepsOverrides {
    pub identity: Option<MemoryCommandIdentity>,
    pub resolve_identity: Option<Option<MemoryCommandIdentity>>,
    pub settings: Option<serde_json::Value>,
    pub config_path: Option<Option<String>>,
    pub sessions_dir: Option<PathBuf>,
    pub reflection_sink: Option<bool>,
    pub dream_sink: Option<bool>,
    pub facts_sink: Option<bool>,
    pub people_ask: Option<bool>,
    pub now_ms: Option<i64>,
}

impl Default for FakeDepsOverrides {
    fn default() -> Self {
        Self {
            identity: None,
            resolve_identity: None,
            settings: None,
            config_path: None,
            sessions_dir: None,
            reflection_sink: None,
            dream_sink: None,
            facts_sink: None,
            people_ask: None,
            now_ms: None,
        }
    }
}

pub fn fake_deps(
    identity: Option<MemoryCommandIdentity>,
    overrides: FakeDepsOverrides,
) -> FakeDeps {
    let busts = Arc::new(Mutex::new(Vec::new()));
    let reflection_requests = Arc::new(Mutex::new(Vec::new()));
    let dream_requests = Arc::new(Mutex::new(Vec::new()));
    let receipt = Arc::new(Mutex::new(("active".to_owned(), "run-1".to_owned())));
    let alive_pids = Arc::new(Mutex::new(BTreeSet::new()));
    let retry_triggers = Arc::new(Mutex::new(Vec::new()));
    let dream_outcome = Arc::new(Mutex::new(DreamCommandOutcome::Fired {
        run_id: "dream-run-1".to_owned(),
        status: "active".to_owned(),
    }));
    let people_asks = Arc::new(Mutex::new(Vec::new()));

    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let actions = Arc::new(FakeActions::new(order));

    let bound_identity = overrides.identity.or_else(|| identity.clone());
    let resolve_bound = bound_identity.clone();
    let resolve_context: crate::prompt::PromptContextResolver = Arc::new(move |_session: &str| {
        resolve_bound.as_ref().map(identity_context)
    });
    let resolve_identity_source = overrides
        .resolve_identity
        .clone()
        .unwrap_or_else(|| identity.clone());
    let resolve_identity: Option<
        Arc<dyn Fn() -> Option<MemoryIdentityContext> + Send + Sync>,
    > = resolve_identity_source.map(|identity| {
        Arc::new(move || Some(identity_context(&identity)))
            as Arc<dyn Fn() -> Option<MemoryIdentityContext> + Send + Sync>
    });

    let settings_value = overrides.settings.clone().unwrap_or_else(memory_settings);
    let settings: Arc<dyn Fn() -> Result<serde_json::Value, String> + Send + Sync> =
        Arc::new(move || Ok(settings_value.clone()));
    let config_path_value = overrides
        .config_path
        .clone()
        .unwrap_or_else(|| Some("/tmp/omo.jsonc".to_owned()));
    let config_path: Option<Arc<dyn Fn() -> Option<String> + Send + Sync>> =
        config_path_value.map(|value| {
            Arc::new(move || Some(value.clone())) as Arc<dyn Fn() -> Option<String> + Send + Sync>
        });

    let busts_sink = busts.clone();
    let bust_prompt_cache: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        busts_sink
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push("bust".to_owned());
    });

    let reflect_requests = reflection_requests.clone();
    let reflect_receipt = receipt.clone();
    let reflect: Option<super::types::Reflect> =
        match overrides.reflection_sink.unwrap_or(true) {
            true => Some(Arc::new(move |_session: &str, event: ReflectionEvent| {
                let request = match event {
                    ReflectionEvent::Manual { focus, recent_n, conversation_ids } => {
                        ManualReflectionRequest { focus, recent_n, conversation_ids }
                    }
                    _ => ManualReflectionRequest::default(),
                };
                reflect_requests
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(request);
                let receipt = reflect_receipt
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                Ok(receipt)
            })),
            false => None,
        };

    let dream: Option<Arc<dyn DreamRequestSink>> = match overrides.dream_sink.unwrap_or(true) {
        true => Some(Arc::new(FakeDreamSink {
            requests: dream_requests.clone(),
            outcome: dream_outcome.clone(),
        })),
        false => None,
    };

    let facts_retry_triggers = retry_triggers.clone();
    let facts_retry: Option<Arc<dyn Fn(String) -> FactsExtractorWork + Send + Sync>> =
        match overrides.facts_sink.unwrap_or(true) {
            true => Some(Arc::new(move |_identity: String| {
                let triggers = facts_retry_triggers.clone();
                Box::pin(async move {
                    triggers
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push("reconcile".to_owned());
                    Ok(())
                }) as FactsExtractorWork
            })),
            false => None,
        };

    let people_asks_sink = people_asks.clone();
    let people_ask: Option<PeopleAskRunner> = match overrides.people_ask.unwrap_or(false) {
        true => Some(Arc::new(move |request: PeopleAskRequest| {
            people_asks_sink
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(request);
            Box::pin(async move { Ok("never reached".to_owned()) })
        })),
        false => None,
    };

    let alive = alive_pids.clone();
    let is_process_alive: Arc<dyn Fn(u32) -> bool + Send + Sync> = Arc::new(move |pid: u32| {
        alive
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&pid)
    });
    let now_ms = overrides.now_ms.unwrap_or(1_782_000_000_000);
    let now: Arc<dyn Fn() -> i64 + Send + Sync> = Arc::new(move || now_ms);

    let deps = MemoryCommandDeps {
        resolve_context,
        resolve_identity,
        settings,
        config_path,
        full_config: Some(Arc::new(|| Ok(serde_json::json!({})))),
        actions: actions.clone(),
        prompt: Arc::new(Default::default()),
        sessions_dir: overrides.sessions_dir.clone().unwrap_or_default(),
        reflect,
        dream,
        facts_retry,
        exec: None,
        env: Some(BTreeMap::new()),
        people_ask,
        now: Some(now),
        is_process_alive: Some(is_process_alive),
    };

    FakeDeps {
        deps,
        busts,
        reflection_requests,
        dream_requests,
        receipt,
        alive_pids,
        retry_triggers,
        dream_outcome,
        people_asks,
        actions,
    }
}

pub fn set_receipt(fake: &FakeDeps, status: &str, run_id: &str) {
    *fake.receipt.lock().unwrap_or_else(PoisonError::into_inner) =
        (status.to_owned(), run_id.to_owned());
}

pub fn set_dream_outcome(fake: &FakeDeps, outcome: DreamCommandOutcome) {
    *fake.dream_outcome.lock().unwrap_or_else(PoisonError::into_inner) = outcome;
}

pub fn reflection_requests(fake: &FakeDeps) -> Vec<ManualReflectionRequest> {
    fake.reflection_requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

pub fn dream_requests(fake: &FakeDeps) -> Vec<ManualDreamCommandRequest> {
    fake.dream_requests
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

pub fn busts(fake: &FakeDeps) -> Vec<String> {
    fake.busts.lock().unwrap_or_else(PoisonError::into_inner).clone()
}

pub fn retry_triggers(fake: &FakeDeps) -> Vec<String> {
    fake.retry_triggers
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

pub fn alive_pids(fake: &FakeDeps) -> Arc<Mutex<BTreeSet<u32>>> {
    fake.alive_pids.clone()
}

pub fn order_of(context: &FakeContext) -> Vec<String> {
    context
        .order
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}
