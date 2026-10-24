//! Default (guarded) permission preset end to end over the faux provider: in-project reads pass,
//! a headless read outside the working directory is refused with an actionable message, and an
//! explicit `--permission-preset full-access` restores the old behavior.

use std::sync::Arc;
use std::time::Duration;

use maho_ai::providers::faux::{FauxAssistantMessageOptions, faux_assistant_message, faux_tool_call};
use maho_ai::types::{AssistantMessage, ContentBlock, Message, StopReason};
use maho_ext_api::{Extension, ExtensionApi, EventKind, EventResult, FlagValue, SourceInfo};
use maho_ext_host::loader::NativeExtensionFactory;
use maho_test_support::faux::FauxScript;
use maho_test_support::faux_session::FauxSession;

const BOUND: Duration = Duration::from_secs(30);

/// The real permission extension, with `--permission-preset` set before it reads its flags.
struct Permission(Option<&'static str>);

impl Extension for Permission {
    fn register(&self, api: &mut ExtensionApi) {
        if let Some(preset) = self.0 {
            let runtime = api.runtime.clone();
            api.on(
                EventKind::SessionStart,
                Arc::new(move |_, _| {
                    let runtime = runtime.clone();
                    Box::pin(async move {
                        runtime.set_flag("permission-preset", FlagValue::String(preset.to_owned()));
                        Ok(EventResult::None)
                    })
                }),
            );
        }
        maho_ext_permission_system::PermissionSystem.register(api);
        let read_def = maho_ext_api::ToolDefinition::new(
            "read",
            "Read file",
            serde_json::json!({"type": "object"}),
            Arc::new(|_| Box::pin(async { Ok(maho_ext_api::ToolResult::text("inside\noutside\n")) })),
        );
        api.try_register_tool(read_def).expect("register read tool");
    }
}

fn read_call(path: &str, id: &str) -> AssistantMessage {
    let arguments = serde_json::json!({ "path": path }).as_object().cloned().expect("object");
    faux_assistant_message(
        faux_tool_call("read", arguments, Some(id)),
        FauxAssistantMessageOptions { stop_reason: Some(StopReason::ToolUse), timestamp: Some(0), ..Default::default() },
    )
}

fn done() -> AssistantMessage {
    faux_assistant_message("done", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() })
}

/// Runs the scripted turns and returns `(is_error, text)` for each tool result, in order.
async fn run(preset: Option<&'static str>, calls: Vec<AssistantMessage>, a_txt_dir_file: bool) -> Vec<(bool, String)> {
    let session = FauxSession::new(FauxScript { name: "permission-default".into(), prompt: "go".into(), responses: vec![] })
        .with_native_extension(NativeExtensionFactory {
            path: "builtin:permission-system".into(),
            source_info: SourceInfo { source: "builtin".into(), ..Default::default() },
            extension: Box::new(Permission(preset)),
        })
        .with_native_responses(calls);
    let handle = tokio::time::timeout(BOUND, session.run_native_handle()).await.expect("boot in time").expect("boot");
    if a_txt_dir_file {
        std::fs::write(handle.cwd().join("a.txt"), "inside\n").expect("a.txt");
    }
    tokio::time::timeout(BOUND, handle.prompt("go".into())).await.expect("prompt in time").expect("join").expect("prompt");
    let results = handle
        .messages()
        .into_iter()
        .filter_map(|message| match message {
            Message::ToolResult(result) => {
                let text = result
                    .content
                    .iter()
                    .filter_map(|block| match block {
                        ContentBlock::Text(text) => Some(text.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Some((result.is_error, text))
            }
            _ => None,
        })
        .collect();
    handle.close().await;
    results
}

fn outside_file() -> (tempfile::TempDir, String) {
    let sibling = tempfile::tempdir().expect("sibling dir");
    let file = sibling.path().join("b.txt");
    std::fs::write(&file, "outside\n").expect("b.txt");
    let path = file.to_string_lossy().into_owned();
    (sibling, path)
}

#[tokio::test]
async fn default_preset_reads_inside_but_refuses_outside_headless() {
    let (_sibling, outside) = outside_file();
    let results = run(None, vec![read_call("./a.txt", "inside"), read_call(&outside, "outside"), done()], true).await;
    assert_eq!(results.len(), 2, "{results:?}");
    assert!(!results[0].0 && results[0].1.contains("inside"), "{:?}", results[0]);
    assert!(results[1].0, "{:?}", results[1]);
    assert!(results[1].1.contains("outside the working directory"), "{:?}", results[1]);
    assert!(results[1].1.contains("--permission external_directory=allow"), "{:?}", results[1]);
}

#[tokio::test]
async fn full_access_preset_reads_outside() {
    let (_sibling, outside) = outside_file();
    let results = run(Some("full-access"), vec![read_call(&outside, "outside"), done()], false).await;
    assert_eq!(results.len(), 1, "{results:?}");
    assert!(!results[0].0 && results[0].1.contains("outside"), "{:?}", results[0]);
}
