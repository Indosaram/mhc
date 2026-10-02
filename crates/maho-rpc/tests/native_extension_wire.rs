use maho_ext_api::*;
use maho_test_support::{faux::{FauxResponse, FauxScript}, faux_session::FauxSession};
use std::sync::{Arc, Mutex};

struct WireExtension(Arc<Mutex<Vec<String>>>);
impl Extension for WireExtension {
    fn register(&self, api: &mut ExtensionApi) {
        for kind in [EventKind::SessionStart, EventKind::AgentStart, EventKind::AgentEnd] {
            let captured = self.0.clone();
            api.on(kind, Arc::new(move |event, context| {
                let captured = captured.clone();
                Box::pin(async move {
                    context.actions()?;
                    captured.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.push(event.kind().as_str().into());
                    Ok(EventResult::None)
                })
            }));
        }
    }
}

#[tokio::test]
async fn rpc_faux_native_extension_events_survive_wire_transformation() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let session = FauxSession::new(FauxScript {
        name: "rpc-extension".into(), prompt: "hello".into(),
        responses: vec![FauxResponse { content: "reply".into(), stop_reason: "stop".into() }],
    }).with_native_extension(maho_ext_host::loader::NativeExtensionFactory {
        path: "<rpc-wire-extension>".into(), source_info: SourceInfo::default(),
        extension: Box::new(WireExtension(captured.clone())),
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.unwrap().unwrap();
    let events = result["events"].as_array().unwrap();
    let wire = events.iter().map(|event| maho_rpc::jsonl::serialize_json_line(maho_rpc::json_event::to_json_event(event).unwrap().as_ref()).unwrap()).collect::<String>();
    assert_eq!(*captured.lock().unwrap(), ["session_start", "agent_start", "agent_end"]);
    assert_eq!(wire.lines().count(), events.len());
    assert_eq!(serde_json::from_str::<serde_json::Value>(wire.lines().last().unwrap()).unwrap()["type"], "agent_end");
}
