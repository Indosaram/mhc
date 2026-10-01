use std::{collections::BTreeSet,path::PathBuf,sync::{Arc,Mutex}};
use maho_ext_api::*;
use crate::{constants::COMMENT_CHECKER_FEEDBACK_HEADER,hook_input::to_hook_input,resolver::resolve_senpi_comment_checker_binary,runner::default_run_comment_checker,utils::normalize_feedback_text};
#[derive(Default)]
struct State { binary:Option<Option<PathBuf>>,reported:BTreeSet<PathBuf> }
pub struct CommentCheckerComponent { pub package_binary:Option<PathBuf> }
impl Extension for CommentCheckerComponent {
    fn register(&self,api:&mut ExtensionApi) {
        let state=Arc::new(Mutex::new(State::default()));let turn_state=Arc::clone(&state);
        api.on(EventKind::TurnStart,Arc::new(move |_,_| { let state=Arc::clone(&turn_state);Box::pin(async move { state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reported.clear();Ok(EventResult::None) }) }));
        let package=self.package_binary.clone();
        api.on(EventKind::ToolResult,Arc::new(move |event,ctx| { let state=Arc::clone(&state);let package=package.clone();Box::pin(async move {
            let ExtensionEvent::ToolResult(event)=event else { return Ok(EventResult::None); };
            if event.is_error || !matches!(event.tool_name.as_str(),"edit"|"write") { return Ok(EventResult::None); }
            let Some(path)=event.input["path"].as_str() else { return Ok(EventResult::None); };
            let absolute=ctx.cwd.join(path);
            let binary={
                let mut state=state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                if state.reported.contains(&absolute) { return Ok(EventResult::None); }
                if state.binary.is_none() { let env=std::env::vars().collect();state.binary=Some(resolve_senpi_comment_checker_binary(&env,package.as_deref())); }
                state.binary.clone().flatten()
            };
            let Some(binary)=binary else { return Ok(EventResult::None); };
            if let Some(update)=&ctx.update_tool_hook_status { update("(OmO) Checking Comments"); }
            let input=comment_checker_core::RunCommentCheckerInput{hook_input:to_hook_input(event,ctx,&absolute.to_string_lossy()),binary_path:Some(binary.to_string_lossy().into_owned()),custom_prompt:None};
            let result=default_run_comment_checker(&input).await.map_err(|e|ExtensionFailure::new(e.to_string()))?;
            let message=normalize_feedback_text(&result.message);
            if !result.has_comments||message.is_empty() { return Ok(EventResult::None); }
            state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reported.insert(absolute.clone());
            let mut content=event.content.clone();content.push(ToolContent::text(format!("{COMMENT_CHECKER_FEEDBACK_HEADER} {}:\n{message}",absolute.display())));
            Ok(EventResult::ToolResult(ToolResultEventResult{content:Some(content),..Default::default()}))
        }) }));
    }
}
