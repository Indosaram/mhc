use maho_ext_api::*;
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::{faux::{FauxResponse, FauxScript}, faux_session::FauxSession};
use std::sync::{Arc, Mutex};

struct NativeInput(Arc<Mutex<Vec<String>>>);
impl Extension for NativeInput {
    fn register(&self, api: &mut ExtensionApi) {
        let sources = self.0.clone();
        api.on(EventKind::Input, Arc::new(move |event, context| {
            let sources = sources.clone();
            Box::pin(async move {
                context.actions()?;
                if let ExtensionEvent::Input(input) = event {
                    sources.lock().map_err(|error| ExtensionFailure::new(error.to_string()))?.push(format!("{:?}", input.source));
                }
                Ok(EventResult::None)
            })
        }));
    }
}

#[tokio::test]
async fn native_faux_extension_observes_real_session_turn_and_projected_items() {
    let sources = Arc::new(Mutex::new(Vec::new()));
    let session = FauxSession::new(FauxScript {
        name: "app-server-native".into(), prompt: "hello".into(),
        responses: vec![FauxResponse { content: "native reply".into(), stop_reason: "stop".into() }],
    }).with_native_extension(NativeExtensionFactory {
        path: "<app-server-native>".into(), source_info: SourceInfo::default(), extension: Box::new(NativeInput(sources.clone())),
    });
    let output = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.unwrap().unwrap();
    assert_eq!(sources.lock().unwrap().len(), 1);
    let mut projector = maho_server::app_server::projection::EventProjector::new("thread".into(), "turn".into(), "/tmp".into());
    let notifications = output["events"].as_array().unwrap().iter().flat_map(|event| projector.project(event).notifications).collect::<Vec<_>>();
    assert!(notifications.iter().any(|notification| notification["method"] == "item/completed" && notification["params"]["item"]["text"] == "native reply"));
    assert_eq!(output["messages"][1]["content"][0]["text"], "native reply");
}
