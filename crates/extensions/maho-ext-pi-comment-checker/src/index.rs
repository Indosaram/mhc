use crate::{cli::{RunStatus, resolve_binary, run_checker}, core::{extract_comment_check_requests, to_hook_input}};
use maho_ext_api::{EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent, NotificationType, ToolContent, ToolResultEventResult};
use std::{path::PathBuf, sync::Arc};

pub struct CommentChecker { pub source_path: PathBuf }
impl Extension for CommentChecker {
    fn register(&self, api: &mut ExtensionApi) {
        api.on(EventKind::SessionStart, Arc::new(|_, ctx| Box::pin(async move {
            ctx.ui.set_widget("pi-comment-checker", None, Default::default());
            Ok(EventResult::None)
        })));
        let source = self.source_path.clone();
        api.on(EventKind::ToolResult, Arc::new(move |event, ctx| {
            let source = source.clone();
            Box::pin(async move {
                let ExtensionEvent::ToolResult(event) = event else { return Ok(EventResult::None); };
                let requests = extract_comment_check_requests(event);
                if requests.is_empty() { return Ok(EventResult::None); }
                let mut warnings = Vec::new();
                let binary = resolve_binary(&source);
                for request in requests {
                    let input = to_hook_input(&request, ctx.session_manager.session_id(), &ctx.cwd.to_string_lossy());
                    let result = run_checker(&input, binary.as_deref()).await;
                    match result.status {
                        RunStatus::Missing | RunStatus::Error => {
                            ctx.ui.set_widget("pi-comment-checker", None, Default::default());
                            return Ok(EventResult::None);
                        }
                        RunStatus::Warning => {
                            if !result.message.trim().is_empty() { warnings.push(result.message.trim().to_owned()); }
                        }
                        RunStatus::Pass => {}
                    }
                }
                ctx.ui.set_widget("pi-comment-checker", None, Default::default());
                if warnings.is_empty() { return Ok(EventResult::None); }
                let mut content = event.content.clone();
                content.extend(warnings.into_iter().map(|warning| ToolContent::text(format!("\n\n{warning}"))));
                Ok(EventResult::ToolResult(ToolResultEventResult { content: Some(content), details: None, is_error: None, usage: None }))
            })
        }));
        let source = self.source_path.clone();
        api.register_command("comment-checker", Some("Show comment-checker extension status and setup guidance.".into()), None, Arc::new(move |_, ctx| {
            let available = resolve_binary(&source).is_some();
            Box::pin(async move {
                ctx.ui.set_widget("pi-comment-checker", None, Default::default());
                if available { ctx.ui.notify("comment-checker binary is available.", NotificationType::Info); }
                else { ctx.ui.notify("comment-checker binary missing; reinstall/reload the extension package.", NotificationType::Warning); }
                Ok(())
            })
        }));
    }
}
