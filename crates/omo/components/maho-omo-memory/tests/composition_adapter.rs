use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use maho_ext_api::*;
use maho_omo_memory::composition::{MemoryExtension, MemoryExtensionOptions};
use maho_omo_memory::facts_wiring::MemoryFactsWiringOptions;
use maho_omo_memory::index::{MemoryComponent, MemoryComponentOptions};
use maho_omo_memory::prompt::{MemoryPromptInjectionOptions, PromptContextResolver};
use maho_omo_memory::wiring::{MemoryWiring, create_memory_wiring};
use maho_omo_memory::wiring_static::MemoryStaticOptions;
use maho_omo_memory::wiring_types::{MemoryWiringOptions, NativeMemoryWiringOptions};

struct Theme;
impl maho_omo_memory::worker::entry_renderers::EntryRenderTheme for Theme {
    fn fg(&self, _: &str, text: &str) -> String {
        text.into()
    }
    fn italic(&self, text: &str) -> String {
        text.into()
    }
}

fn wiring_options() -> NativeMemoryWiringOptions {
    NativeMemoryWiringOptions {
        facts: Arc::new(|identity, _ctx| {
            Ok(MemoryFactsWiringOptions {
                identity: identity.identity.clone(),
                identity_paths: identity.identity_paths.clone(),
                facts_enabled: Box::new(|| false),
                debounce_settles: Box::new(|| 4),
                extractor: None,
                now: Some(Arc::new(|| 0)),
                warn: Arc::new(|_| {}),
            })
        }),
        reconcile: Arc::new(|_| Box::pin(async { Ok(()) })),
        initial_status: Arc::new(|_, _| {}),
        now: Arc::new(|| 0.0),
        warn: Arc::new(|_| {}),
    }
}

fn static_options() -> MemoryStaticOptions {
    let resolve: PromptContextResolver = Arc::new(|_| None);
    MemoryStaticOptions {
        register_host: None,
        skills_trackers: None,
        prompt_handler: None,
        write_notice: None,
        tool_exposure: None,
        prompt: MemoryPromptInjectionOptions {
            resolve_context: resolve.clone(),
            create_repo: None,
            search_exposure: None,
            resolve_nudge_turns: None,
            resolve_soul_notice: None,
        },
        nudge: Arc::new(Mutex::new(Default::default())),
        resolve_context: resolve,
        resolve_tool_context: Arc::new(|| None),
        resolve_cwd: Arc::new(PathBuf::new),
        captured_tools: Arc::new(Vec::new),
        warn: Arc::new(|_| {}),
        edit_notice: Arc::new(|_| true),
        theme: Arc::new(|_| Arc::new(Theme)),
        memory_write: maho_omo_memory::wiring_memory_write::MemoryWriteOptions {
            resolve_session: Arc::new(|_| None),
            on_memory_write: Arc::new(|_| Ok(())),
            refresh_status: Arc::new(|_, _, _| panic!("unbound")),
            sync_status: None,
        },
        skills_usage: maho_omo_memory::skills_usage_wiring::SkillsUsageOptions {
            resolve_context: Arc::new(|_| None),
            resolve_cwd: Arc::new(PathBuf::new),
            now_ms: Arc::new(|| 0),
        },
        triggers: maho_omo_memory::trigger_wiring::ReflectionTriggerWiringOptions {
            resolve_session: Arc::new(|_| None),
            on_launch: Arc::new(|_| Ok(())),
            warn: Arc::new(|_| {}),
        },
        device_name: None,
        load_settings: None,
    }
}

fn extension(root: &std::path::Path) -> (MemoryExtension, Arc<MemoryComponent>) {
    let component = MemoryComponent::new(MemoryComponentOptions {
        env: std::collections::BTreeMap::from([(
            "HOME".to_owned(),
            root.to_string_lossy().into_owned(),
        )]),
        cwd: root.into(),
        now: Arc::new(|| 0.0),
        disabled: Arc::new(|| false),
        load_config: Arc::new(|| Ok(serde_json::json!({}))),
        sync_runner: None,
    });
    let wiring: Arc<tokio::sync::Mutex<MemoryWiring>> = Arc::new(tokio::sync::Mutex::new(
        create_memory_wiring(MemoryWiringOptions {
            runtime: Default::default(),
            skills_usage: Default::default(),
        }),
    ));
    (
        MemoryExtension::new(MemoryExtensionOptions {
            component: component.clone(),
            wiring,
            wiring_options: wiring_options(),
            static_options: static_options(),
        }),
        component,
    )
}

fn api(root: &std::path::Path) -> ExtensionApi {
    ExtensionApi::new(
        LoadedExtension::new("memory", root.into(), SourceInfo::default()),
        ExtensionSessionProfile::default(),
        EventBus::default(),
        ExtensionRuntime::default(),
    )
}

fn session_start_count(api: &ExtensionApi) -> usize {
    api.registered
        .handlers
        .get(&EventKind::SessionStart)
        .map_or(0, Vec::len)
}

#[test]
fn re_registers_into_each_new_runner_and_reuses_the_single_store() {
    let root = tempfile::tempdir().expect("private root");
    let (extension, component) = extension(root.path());
    let mut first = api(root.path());
    extension.register(&mut first);
    let starts = session_start_count(&first);
    assert!(starts >= 1, "memory must install its lifecycle handlers");
    assert!(
        first
            .registered
            .handlers
            .get(&EventKind::ToolCall)
            .map_or(0, Vec::len)
            >= 1,
        "memory must install its guard handler"
    );
    assert!(
        first
            .registered
            .rpc_handlers
            .contains_key(maho_omo_memory::memory_rpc_bridge::MEMORY_STATUS_RPC_METHOD),
        "memory must serve the omo.memory.status pull request"
    );
    let mut second = api(root.path());
    extension.register(&mut second);
    assert_eq!(
        session_start_count(&second),
        starts,
        "a new runner must receive the same handlers (reload rebind, not a no-op)"
    );
    assert!(
        Arc::ptr_eq(extension.component(), &component),
        "the adapter must reuse the composition's single store"
    );
    assert!(
        component
            .sessions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty(),
        "no session is bound before SessionStart"
    );
    assert!(!root.path().join("repo").exists());
}
