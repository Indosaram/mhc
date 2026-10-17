use std::sync::{Arc, Mutex};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, SourceInfo};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};
use maho_omo_init_deep_advisor::component::InitDeepAdvisorComponent;

struct ObservedComponent<T> {
    component: T,
    events: Arc<Mutex<Vec<&'static str>>>,
}

struct DeferredAdvisor {
    component: InitDeepAdvisorComponent,
    preflights: Arc<std::sync::atomic::AtomicUsize>,
}

impl Extension for DeferredAdvisor {
    fn register(&self, api: &mut ExtensionApi) {
        self.component.register(api);
        let handlers = api.registered.handlers.remove(&EventKind::SessionStart).expect("startup handler");
        api.registered.handler_options.remove(&EventKind::SessionStart);
        for original in handlers {
            let preflights = Arc::clone(&self.preflights);
            api.on(EventKind::SessionStart, Arc::new(move |event, ctx| {
                let original = Arc::clone(&original);
                let preflights = Arc::clone(&preflights);
                let mut ctx = ctx.clone();
                let queued = Arc::new(Mutex::new(Vec::<Box<dyn FnOnce() + Send>>::new()));
                let tasks = Arc::clone(&queued);
                ctx.defer_macrotask = Some(Arc::new(move |task| tasks.lock().expect("queue").push(task)));
                Box::pin(async move {
                    let result = original(event, &ctx).await;
                    assert_eq!(preflights.load(std::sync::atomic::Ordering::SeqCst), 0);
                    let tasks = std::mem::take(&mut *queued.lock().expect("queue"));
                    assert_eq!(tasks.len(), 1);
                    for task in tasks { task(); }
                    assert_eq!(preflights.load(std::sync::atomic::Ordering::SeqCst), 1);
                    result
                })
            }));
        }
    }
}

#[tokio::test]
async fn registered_startup_defers_preflight_until_host_callback_runs() {
    let state = tempfile::tempdir().expect("private state");
    let preflights = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = Arc::clone(&preflights);
    let component = InitDeepAdvisorComponent {
        state_dir: state.path().into(), skills_root: state.path().join("skills"), process_start: 2.0,
        marker_mtime: Arc::new(move |_| { counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst); None }),
    };
    let session = FauxSession::new(load_script("hello").expect("script")).with_native_extension(NativeExtensionFactory {
        path: "<deferred-advisor>".into(), source_info: SourceInfo::default(),
        extension: Box::new(DeferredAdvisor { component, preflights }),
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.expect("bounded session").expect("session");
}

impl<T: Extension> Extension for ObservedComponent<T> {
    fn register(&self, api: &mut ExtensionApi) {
        self.component.register(api);
        for kind in [EventKind::SessionStart, EventKind::TurnEnd, EventKind::SessionShutdown] {
            let events = Arc::clone(&self.events);
            api.on(kind, Arc::new(move |_, _| {
                events.lock().expect("lifecycle events").push(kind.as_str());
                Box::pin(async { Ok(EventResult::None) })
            }));
        }
    }
}

#[tokio::test]
async fn faux_component_runs_real_session_startup_and_turn() {
    let state = tempfile::tempdir().expect("private component state");
    let events = Arc::new(Mutex::new(Vec::new()));
    let script = load_script("hello").expect("shared reference script");
    let prompt = script.prompt.clone();
    let response = script.responses[0].content.clone();
    let component = InitDeepAdvisorComponent { state_dir: state.path().into(), skills_root: state.path().join("skills"), marker_mtime: Arc::new(|_| None), process_start: 2.0 };
    let session = FauxSession::new(script).with_native_extension(NativeExtensionFactory {
        path: "<todo-46-init-deep-advisor>".into(),
        source_info: SourceInfo { source: "inline".into(), ..Default::default() },
        extension: Box::new(ObservedComponent { component, events: Arc::clone(&events) }),
    });
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("native component turn settles").expect("native component session");
    let messages = output["messages"].as_array().expect("session messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["content"][0]["text"], prompt);
    assert_eq!(messages[1]["content"][0]["text"], response);
    let reference: serde_json::Value = serde_json::from_str(include_str!("reference_hello.json")).expect("generated reference");
    let expected = reference["entries"].as_array().unwrap().iter().filter_map(|entry| {
        let message = &entry["message"];
        matches!(message["role"].as_str(), Some("user" | "assistant")).then(|| message["content"].clone())
    }).collect::<Vec<_>>();
    assert_eq!(messages.iter().map(|message| message["content"].clone()).collect::<Vec<_>>(), expected);
    assert_eq!(*events.lock().expect("lifecycle events"), ["session_start", "turn_end", "session_shutdown"]);
    assert!(std::fs::read_dir(state.path()).expect("component state").next().is_none());
}
