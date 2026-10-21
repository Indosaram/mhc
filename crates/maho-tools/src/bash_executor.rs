use crate::{
    ansi::strip_ansi,
    bash::{BashDataCallback, BashExecOptions, BashOperations},
    definition::*,
    output_accumulator::{OutputAccumulator, OutputAccumulatorOptions},
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
pub type BashChunkCallback = Arc<dyn Fn(&str) -> Result<(), ToolError> + Send + Sync>;
pub type BashAsyncChunkCallback = Arc<dyn Fn(String) -> ToolFuture<'static, ()> + Send + Sync>;
#[derive(Default)]
pub struct BashExecutorOptions {
    pub on_chunk: Option<BashChunkCallback>,
    pub on_chunk_async: Option<BashAsyncChunkCallback>,
    pub signal: AbortSignal,
    pub env: Option<BTreeMap<String, String>>,
}
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BashResult {
    pub output: String,
    pub exit_code: Option<i32>,
    pub cancelled: bool,
    pub truncated: bool,
    pub full_output_path: Option<PathBuf>,
}
pub async fn execute_bash_with_operations(
    command: &str,
    cwd: &Path,
    operations: &dyn BashOperations,
    options: BashExecutorOptions,
) -> Result<BashResult, ToolError> {
    let output = Arc::new(Mutex::new(OutputAccumulator::new(
        OutputAccumulatorOptions {
            temp_file_prefix: "pi-bash".into(),
            ..Default::default()
        },
    )));
    let captured = Arc::clone(&output);
    let callback = options.on_chunk;
    let callback_error = Arc::new(Mutex::new(None));
    let data_error = Arc::clone(&callback_error);
    let execution_signal = AbortSignal::default();
    if options.signal.is_aborted() {
        execution_signal.abort();
    }
    let callback_signal = execution_signal.clone();
    let callbacks = Arc::new(Mutex::new(tokio::task::JoinSet::new()));
    let pending_callbacks = Arc::clone(&callbacks);
    let async_callback = options.on_chunk_async;
    let pending = Arc::new(Mutex::new(Vec::<u8>::new()));
    let pending_callback = Arc::clone(&pending);
    let on_data: BashDataCallback = Arc::new(move |data| {
        if data_error
            .lock()
            .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))?
            .is_some()
        {
            return Ok(());
        }
        let mut pending = pending_callback
            .lock()
            .map_err(|_| ToolError::Message("Bash decoder lock poisoned".into()))?;
        pending.extend_from_slice(data);
        let mut consumed = 0;
        let mut decoded = String::new();
        while consumed < pending.len() {
            match std::str::from_utf8(&pending[consumed..]) {
                Ok(text) => {
                    decoded.push_str(text);
                    consumed = pending.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    decoded.push_str(&String::from_utf8_lossy(&pending[consumed..end]));
                    consumed = end;
                    if let Some(length) = error.error_len() {
                        decoded.push('\u{fffd}');
                        consumed += length;
                    } else {
                        break;
                    }
                }
            }
        }
        pending.drain(..consumed);
        drop(pending);
        let text: String = strip_ansi(&decoded)
            .chars()
            .filter(|c| {
                (*c > '\u{1f}' || *c == '\n' || *c == '\t')
                    && !('\u{fff9}'..='\u{fffb}').contains(c)
            })
            .collect();
        captured
            .lock()
            .map_err(|_| ToolError::Message("Bash output lock poisoned".into()))?
            .append_text(&text)?;
        if let Some(callback) = &callback
            && let Err(error) = callback(&text)
        {
            let message = error.to_string();
            *data_error
                .lock()
                .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))? =
                Some(error);
            callback_signal.abort();
            return Err(ToolError::Message(message));
        }
        if let Some(callback) = &async_callback {
            let future = callback(text);
            let errors = Arc::clone(&data_error);
            let signal = callback_signal.clone();
            pending_callbacks
                .lock()
                .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))?
                .spawn(async move {
                    if let Err(error) = future.await {
                        let mut errors = errors.lock().map_err(|_| {
                            ToolError::Message("Bash callback lock poisoned".into())
                        })?;
                        if errors.is_none() {
                            *errors = Some(error);
                        }
                        signal.abort();
                    }
                    Ok(())
                });
        }
        Ok(())
    });
    let operation = operations.exec(
        command,
        cwd,
        BashExecOptions {
            on_data: Arc::clone(&on_data),
            signal: execution_signal.clone(),
            timeout: None,
            env: options.env.unwrap_or_else(|| std::env::vars().collect()),
        },
    );
    tokio::pin!(operation);
    let execution = loop {
        tokio::select! {
            result = &mut operation => break result,
            () = options.signal.cancelled(), if !execution_signal.is_aborted() => execution_signal.abort(),
        }
    };
    let tail = {
        let mut pending = pending
            .lock()
            .map_err(|_| ToolError::Message("Bash decoder lock poisoned".into()))?;
        let tail = String::from_utf8_lossy(&pending).into_owned();
        pending.clear();
        tail
    };
    if !tail.is_empty()
        && let Err(error) = on_data(tail.as_bytes())
    {
        *callback_error
            .lock()
            .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))? = Some(error);
    }
    let mut callback_tasks = std::mem::take(
        &mut *callbacks
            .lock()
            .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))?,
    );
    let deadline = tokio::time::Instant::now()
        + std::time::Duration::from_millis(if options.signal.is_aborted() {
            100
        } else {
            5000
        });
    while !callback_tasks.is_empty() {
        match tokio::time::timeout_at(deadline, callback_tasks.join_next()).await {
            Ok(Some(Ok(Ok(())))) => {}
            Ok(Some(Ok(Err(error)))) => {
                *callback_error
                    .lock()
                    .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))? =
                    Some(error);
            }
            Ok(Some(Err(error))) => {
                *callback_error
                    .lock()
                    .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))? =
                    Some(ToolError::Message(error.to_string()));
            }
            Ok(None) => break,
            Err(_) => {
                if !options.signal.is_aborted() {
                    let mut error = callback_error
                        .lock()
                        .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))?;
                    if error.is_none() {
                        *error = Some(ToolError::Message(
                            "Bash output callback did not settle within 5000ms".into(),
                        ));
                    }
                }
                callback_tasks.abort_all();
                break;
            }
        }
    }
    let mut output = output
        .lock()
        .map_err(|_| ToolError::Message("Bash output lock poisoned".into()))?;
    let observer_error = callback_error
        .lock()
        .map_err(|_| ToolError::Message("Bash callback lock poisoned".into()))?
        .take();
    let snapshot = match output.finish_output() {
        Ok(snapshot) => snapshot,
        Err(finalization) => {
            return Err(match observer_error.or_else(|| execution.err()) {
                Some(primary) => ToolError::Message(format!(
                    "{primary}; Bash output finalization failed: {finalization}"
                )),
                None => finalization,
            });
        }
    };
    if let Some(error) = observer_error {
        return Err(output.cleanup_error(error));
    }
    let cancelled = options.signal.is_aborted();
    match execution {
        Ok(exit) => Ok(BashResult {
            output: snapshot.content,
            exit_code: if cancelled { None } else { exit.exit_code },
            cancelled,
            truncated: snapshot.truncation.truncated,
            full_output_path: snapshot.full_output_path,
        }),
        Err(_) if cancelled => {
            output.remove_temp_file()?;
            Ok(BashResult {
                output: snapshot.content,
                exit_code: None,
                cancelled: true,
                truncated: snapshot.truncation.truncated,
                full_output_path: None,
            })
        }
        Err(error) => Err(output.cleanup_error(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bash::BashExit;

    struct UntilCancelled;
    impl BashOperations for UntilCancelled {
        fn exec<'a>(
            &'a self,
            _: &'a str,
            _: &'a Path,
            options: BashExecOptions,
        ) -> ToolFuture<'a, BashExit> {
            Box::pin(async move {
                (options.on_data)(b"output")?;
                options.signal.cancelled().await;
                Err(ToolError::Aborted)
            })
        }
    }

    #[tokio::test]
    async fn async_observer_failure_cancels_live_operation_not_caller() {
        let caller = AbortSignal::default();
        let options = BashExecutorOptions {
            signal: caller.clone(),
            on_chunk_async: Some(Arc::new(|_| {
                Box::pin(async { Err(ToolError::Message("observer failed".into())) })
            })),
            ..Default::default()
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            execute_bash_with_operations("ignored", Path::new("."), &UntilCancelled, options),
        )
        .await
        .expect("observer cancels command");
        assert!(matches!(result, Err(ToolError::Message(_))));
        assert!(!caller.is_aborted());
    }
}
