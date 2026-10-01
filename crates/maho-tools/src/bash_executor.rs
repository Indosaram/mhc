use std::{collections::BTreeMap, path::{Path, PathBuf}, sync::{Arc, Mutex}};
use crate::{bash::{BashOperations, BashExecOptions, BashDataCallback}, definition::*, output_accumulator::{OutputAccumulator, OutputAccumulatorOptions}};
pub type BashChunkCallback = Arc<dyn Fn(&str) -> Result<(), ToolError> + Send + Sync>;
#[derive(Default)]
pub struct BashExecutorOptions { pub on_chunk: Option<BashChunkCallback>, pub signal: AbortSignal }
pub struct BashResult { pub output: String, pub exit_code: Option<i32>, pub cancelled: bool, pub truncated: bool, pub full_output_path: Option<PathBuf> }
pub async fn execute_bash_with_operations(command: &str, cwd: &Path, operations: &dyn BashOperations, options: BashExecutorOptions) -> Result<BashResult, ToolError> {
    let output = Arc::new(Mutex::new(OutputAccumulator::new(OutputAccumulatorOptions { temp_file_prefix: "pi-bash".into(), ..Default::default() })));
    let captured = Arc::clone(&output); let callback = options.on_chunk;
    let pending = Arc::new(Mutex::new(Vec::<u8>::new()));
    let pending_callback = Arc::clone(&pending);
    let ansi = regex::Regex::new(r"\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))").map_err(|e| ToolError::Message(e.to_string()))?;
    let on_data: BashDataCallback = Arc::new(move |data| {
        let mut pending = pending_callback.lock().map_err(|_| ToolError::Message("Bash decoder lock poisoned".into()))?;
        pending.extend_from_slice(data);
        let mut consumed = 0; let mut decoded = String::new();
        while consumed < pending.len() {
            match std::str::from_utf8(&pending[consumed..]) {
                Ok(text) => { decoded.push_str(text); consumed = pending.len(); },
                Err(error) => {
                    let end = consumed+error.valid_up_to(); decoded.push_str(&String::from_utf8_lossy(&pending[consumed..end])); consumed = end;
                    if let Some(length) = error.error_len() { decoded.push('\u{fffd}'); consumed += length; } else { break; }
                }
            }
        }
        pending.drain(..consumed); drop(pending);
        let text: String = ansi.replace_all(&decoded, "").chars().filter(|c| (*c > '\u{1f}' || *c == '\n' || *c == '\t') && !('\u{fff9}'..='\u{fffb}').contains(c)).collect();
        captured.lock().map_err(|_| ToolError::Message("Bash output lock poisoned".into()))?.append_text(&text)?;
        if let Some(callback) = &callback { callback(&text)?; } Ok(())
    });
    let execution = operations.exec(command, cwd, BashExecOptions { on_data, signal: options.signal.clone(), timeout: None, env: std::env::vars().collect::<BTreeMap<_,_>>() }).await;
    let mut output = output.lock().map_err(|_| ToolError::Message("Bash output lock poisoned".into()))?;
    let tail = String::from_utf8_lossy(&pending.lock().map_err(|_| ToolError::Message("Bash decoder lock poisoned".into()))?).into_owned();
    if !tail.is_empty() { output.append_text(&tail)?; }
    output.finish()?; let snapshot = output.snapshot(true)?; output.close_temp_file()?;
    let cancelled = options.signal.is_aborted();
    match execution {
        Ok(exit) => Ok(BashResult { output: snapshot.content, exit_code: if cancelled { None } else { exit.exit_code }, cancelled, truncated: snapshot.truncation.truncated, full_output_path: snapshot.full_output_path }),
        Err(_) if cancelled => { output.remove_temp_file()?; Ok(BashResult { output: snapshot.content, exit_code: None, cancelled: true, truncated: snapshot.truncation.truncated, full_output_path: None }) },
        Err(error) => { output.remove_temp_file()?; Err(error) },
    }
}
