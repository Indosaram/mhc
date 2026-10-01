use std::{path::Path, sync::{Arc, Mutex}};
use maho_tools::{bash::*, bash_executor::*, definition::*};
use serde_json::json;

struct Chunks(Vec<Vec<u8>>);
impl BashOperations for Chunks {
    fn exec<'a>(&'a self, _: &'a str, _: &'a Path, options: BashExecOptions) -> ToolFuture<'a, BashExit> {
        Box::pin(async move {
            for chunk in &self.0 { (options.on_data)(chunk)?; }
            Ok(BashExit { exit_code: Some(0) })
        })
    }
}

#[tokio::test]
async fn executor_sanitizes_and_flushes_split_utf8() {
    let chunks = Chunks(vec![b"\x1b[31mred\x1b[0m\r\n\0".to_vec(), vec![0xed, 0x95], vec![0x9c], vec![0xe2]]);
    let observed = Arc::new(Mutex::new(String::new()));
    let callback_output = Arc::clone(&observed);
    let result = execute_bash_with_operations("ignored", Path::new("."), &chunks, BashExecutorOptions {
        on_chunk: Some(Arc::new(move |text| { callback_output.lock().unwrap().push_str(text); Ok(()) })),
        ..Default::default()
    }).await.unwrap();
    assert_eq!(result.output, "red\n한\u{fffd}");
    assert_eq!(*observed.lock().unwrap(), result.output);
    assert_eq!(result.exit_code, Some(0));
}

#[tokio::test]
async fn shell_flushes_final_update_and_full_spill() {
    let data = "x".repeat(60_000);
    let updates = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&updates);
    let tool = create_bash_tool_definition(".".into(), BashToolOptions {
        operations: Some(Arc::new(Chunks(vec![data.as_bytes().to_vec()]))), ..Default::default()
    });
    let result = (tool.execute)(ToolCall {
        id: "spill", params: json!({"command":"ignored"}), signal: AbortSignal::default(),
        on_update: Some(Arc::new(move |result| { captured.lock().unwrap().push(result); Ok(()) })), context: None
    }).await.unwrap();
    let updates = updates.lock().unwrap();
    assert!(updates[0].content.is_empty());
    let final_update = updates.last().unwrap();
    let path = final_update.details.as_ref().unwrap()["fullOutputPath"].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(path).unwrap(), data);
    assert_eq!(result.details.as_ref().unwrap()["fullOutputPath"], path);
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn observer_error_cleans_spill() {
    let path = Arc::new(Mutex::new(None));
    let captured = Arc::clone(&path);
    let tool = create_bash_tool_definition(".".into(), BashToolOptions {
        operations: Some(Arc::new(Chunks(vec![vec![b'x'; 60_000]]))), ..Default::default()
    });
    let error = (tool.execute)(ToolCall {
        id: "observer", params: json!({"command":"ignored"}), signal: AbortSignal::default(),
        on_update: Some(Arc::new(move |result| {
            if let Some(details) = result.details {
                *captured.lock().unwrap() = details["fullOutputPath"].as_str().map(str::to_owned);
                return Err(ToolError::Message("observer failed".into()));
            }
            Ok(())
        })), context: None
    }).await.unwrap_err();
    assert_eq!(error.to_string(), "observer failed");
    assert!(!Path::new(path.lock().unwrap().as_ref().unwrap()).exists());
}

#[tokio::test]
async fn executor_observer_error_propagates() {
    let error = execute_bash_with_operations("ignored", Path::new("."), &Chunks(vec![vec![b'x'; 60_000]]), BashExecutorOptions {
        on_chunk: Some(Arc::new(|_| Err(ToolError::Message("observer failed".into())))), ..Default::default()
    }).await.err().unwrap();
    assert_eq!(error.to_string(), "observer failed");
}

#[test]
fn timeout_rejects_invalid_or_overflowing_values() {
    for seconds in [0.0, -1.0, f64::INFINITY, f64::NAN, 2_147_483.648] {
        assert!(resolve_timeout_ms(Some(seconds)).is_err());
    }
    assert_eq!(resolve_timeout_ms(Some(0.5)).unwrap().unwrap().as_millis(), 500);
}

#[tokio::test(start_paused = true)]
async fn executor_bounds_unsettled_async_observer() {
    let result = execute_bash_with_operations("ignored", Path::new("."), &Chunks(vec![b"output".to_vec()]), BashExecutorOptions {
        on_chunk_async: Some(Arc::new(|_| Box::pin(std::future::pending()))), ..Default::default()
    }).await;
    assert_eq!(result.err().unwrap().to_string(), "Bash output callback did not settle within 5000ms");
}

#[tokio::test]
async fn executor_awaits_async_observer_signal() {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let receiver = Arc::new(Mutex::new(Some(receiver)));
    let (started, observed) = tokio::sync::oneshot::channel();
    let started = Arc::new(Mutex::new(Some(started)));
    let options = BashExecutorOptions {
        on_chunk_async: Some(Arc::new(move |_| {
            let receiver = receiver.lock().unwrap().take().unwrap();
            let started = started.lock().unwrap().take().unwrap();
            Box::pin(async move { started.send(()).unwrap(); receiver.await.unwrap(); Ok(()) })
        })), ..Default::default()
    };
    let execution = tokio::spawn(async move { execute_bash_with_operations("ignored", Path::new("."), &Chunks(vec![b"output".to_vec()]), options).await });
    tokio::time::timeout(std::time::Duration::from_secs(1), observed).await.unwrap().unwrap();
    assert!(!execution.is_finished());
    sender.send(()).unwrap();
    assert_eq!(execution.await.unwrap().unwrap().output, "output");
}
