//! Acquisition hooks for the anthropic-subscription provider.
pub mod acquire;

use acquire::{ClaudeCodePin, acquire_claude_code, is_executable};
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, NotificationType,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub fn read_claude_code_pin(runtime: &Path) -> Result<Option<ClaudeCodePin>, String> {
    match std::fs::read(runtime.join("claude-code-pin.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn installed() -> bool {
    if maho_ext_api::process_env::var("CLAUDE_CODE_EXECUTABLE")
        .is_some_and(|value| !value.is_empty())
    {
        return true;
    }
    maho_ext_api::process_env::var("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| is_executable(&dir.join(acquire::executable_name())))
    })
}

#[derive(Default)]
pub struct ClaudeCodeComponent;

impl Extension for ClaudeCodeComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let runtime = maho_ext_api::process_env::var("OMO_PACKAGE_DIR")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::current_exe()
                    .ok()?
                    .parent()
                    .map(Path::to_path_buf)
            });
        let Some(runtime) = runtime else {
            return;
        };
        let pin = match read_claude_code_pin(&runtime) {
            Ok(Some(pin)) => pin,
            Ok(None) => return,
            Err(error) => {
                maho_ext_api::diagnostic(&format!("Claude Code pin: {error}"));
                return;
            }
        };
        let root = runtime.join("claude-code");
        let lock = Arc::new(tokio::sync::Mutex::new(()));
        for kind in [EventKind::Input, EventKind::BeforeAgentStart] {
            let pin = pin.clone();
            let root = root.clone();
            let lock = lock.clone();
            api.on(kind, Arc::new(move |event, ctx| {
                let pin = pin.clone();
                let root = root.clone();
                let lock = lock.clone();
                Box::pin(async move {
                    if ctx.model.as_ref().is_none_or(|model| model.provider != "anthropic-subscription")
                        || matches!(event, ExtensionEvent::BeforeAgentStart(payload) if payload.preview)
                    { return Ok(EventResult::None); }
                    let _guard = lock.lock().await;
                    if !installed() {
                        match acquire_claude_code(&root, &pin).await {
                            Ok(path) => maho_ext_api::process_env::set("CLAUDE_CODE_EXECUTABLE", &path.to_string_lossy()),
                            Err(error) => {
                                ctx.ui.notify(&error, NotificationType::Error);
                                maho_ext_api::diagnostic(&error);
                            }
                        }
                    }
                    Ok(EventResult::None)
                })
            }));
        }
    }
}
