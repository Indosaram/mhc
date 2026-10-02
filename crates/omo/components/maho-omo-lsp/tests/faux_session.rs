use std::sync::{Arc, Mutex};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, SourceInfo};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::load_script, faux_session::FauxSession};
use maho_omo_lsp::LspComponent;

struct ObservedComponent<T> {
    component: T,
    events: Arc<Mutex<Vec<&'static str>>>,
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
    let component = LspComponent;
    let session = FauxSession::new(script).with_native_extension(NativeExtensionFactory {
        path: "<todo-46-lsp>".into(),
        source_info: SourceInfo { source: "inline".into(), ..Default::default() },
        extension: Box::new(ObservedComponent { component, events: Arc::clone(&events) }),
    });
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native())
        .await.expect("native component turn settles").expect("native component session");
    let messages = output["messages"].as_array().expect("session messages");
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0]["content"][0]["text"], prompt);
    assert_eq!(messages[1]["content"][0]["text"], response);
    assert_eq!(*events.lock().expect("lifecycle events"), ["session_start", "turn_end"]);
    assert!(std::fs::read_dir(state.path()).expect("component state").next().is_none());
}
