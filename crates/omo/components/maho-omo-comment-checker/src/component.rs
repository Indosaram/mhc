use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use maho_ext_api::*;

use crate::constants::COMMENT_CHECKER_FEEDBACK_HEADER;
use crate::downloader::download_senpi_comment_checker_binary;
use crate::failure_notice::report_comment_checker_failure;
use crate::hook_input::{to_apply_patch_hook_inputs, to_hook_input};
use crate::resolver::resolve_senpi_comment_checker_binary;
use crate::runner::default_run_comment_checker;
use crate::types::SenpiCommentCheckerDownloadOptions;
use crate::utils::normalize_feedback_text;

#[derive(Default)]
struct State {
    binary: Option<Option<PathBuf>>,
    inert_for_session: bool,
    missing_binary_notice_logged: bool,
    reported: BTreeSet<PathBuf>,
}

#[derive(Default)]
pub struct CommentCheckerComponent {
    pub package_binary: Option<PathBuf>,
    pub options: crate::types::CommentCheckerComponentOptions,
}

impl Extension for CommentCheckerComponent {
    fn register(&self, api: &mut ExtensionApi) {
        let state = Arc::new(Mutex::new(State::default()));
        // Single-flight binary resolution (ensureBinary): held across awaits, so it lives outside the std Mutex.
        let ensuring_binary = Arc::new(tokio::sync::Mutex::new(()));
        let turn_state = Arc::clone(&state);
        api.on(
            EventKind::TurnStart,
            Arc::new(move |_, _| {
                let state = Arc::clone(&turn_state);
                Box::pin(async move {
                    state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .reported
                        .clear();
                    Ok(EventResult::None)
                })
            }),
        );

        let package = self.package_binary.clone();
        let resolve = self.options.resolve_binary.clone();
        let download = self.options.download_binary.clone();
        let check = self.options.run_comment_checker.clone();

        api.on(
            EventKind::ToolResult,
            Arc::new(move |event, ctx| {
                let state = Arc::clone(&state);
                let ensuring_binary = Arc::clone(&ensuring_binary);
                let package = package.clone();
                let resolve = resolve.clone();
                let download = download.clone();
                let check = check.clone();
                Box::pin(async move {
                    let ExtensionEvent::ToolResult(event) = event else {
                        return Ok(EventResult::None);
                    };
                    if event.is_error || !matches!(event.tool_name.as_str(), "edit" | "write" | "apply_patch") {
                        return Ok(EventResult::None);
                    }
                    let patch_inputs = if event.tool_name == "apply_patch" { to_apply_patch_hook_inputs(event, ctx) } else { Vec::new() };
                    let paths = if event.tool_name == "apply_patch" {
                        patch_inputs.iter().filter_map(|(_, input)| input.tool_input.file_path.as_ref().map(PathBuf::from)).collect::<Vec<_>>()
                    } else if let Some(path) = event.input["path"].as_str() {
                        vec![normalize_path(&ctx.cwd.join(path))]
                    } else { return Ok(EventResult::None); };
                    if paths.is_empty() { return Ok(EventResult::None); }

                    // Check if already reported or if session is inert
                    {
                        let s = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        if s.inert_for_session || paths.iter().all(|path| s.reported.contains(path)) {
                            return Ok(EventResult::None);
                        }
                    }

                    // Ensure binary path (cached per session, downloaded if missing)
                    let cached_binary = {
                        let s = state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        s.binary.clone()
                    };

                    let binary = match cached_binary {
                        Some(b) => b,
                        None => {
                            let _ensuring = ensuring_binary.lock().await;
                            if let Some(binary) = state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).binary.clone() {
                                binary
                            } else {
                            let resolved = match &resolve {
                                Some(r) => r(),
                                None => {
                                    let env = std::env::vars().collect();
                                    resolve_senpi_comment_checker_binary(
                                        &env,
                                        package.as_deref(),
                                    )
                                }
                            };

                            let final_binary = match resolved {
                                Some(b) => Some(b),
                                None => match &download {
                                    Some(d) => d(ctx.logger.clone()).await,
                                    None => {
                                        download_senpi_comment_checker_binary(
                                            &SenpiCommentCheckerDownloadOptions {
                                                logger: ctx.logger.clone(),
                                                ..Default::default()
                                            },
                                        )
                                        .await
                                    }
                                },
                            };

                            let mut s = state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            s.binary = Some(final_binary.clone());
                            if final_binary.is_none() {
                                s.inert_for_session = true;
                                if !s.missing_binary_notice_logged {
                                    if let Some(logger) = &ctx.logger {
                                        logger.warn(
                                            "omo-senpi comment-checker binary unavailable; component disabled for this session",
                                            None,
                                        );
                                    }
                                    s.missing_binary_notice_logged = true;
                                }
                            }
                            final_binary
                            }
                        }
                    };

                    let Some(binary) = binary else {
                        return Ok(EventResult::None);
                    };

                    if let Some(update) = &ctx.update_tool_hook_status {
                        update("(OmO) Checking Comments");
                    }
                    let mut content = event.content.clone();
                    let inputs = if event.tool_name == "apply_patch" {
                        patch_inputs.into_iter().map(|(_, hook_input)| hook_input).collect::<Vec<_>>()
                    } else {
                        vec![to_hook_input(event, ctx, &paths[0].to_string_lossy())]
                    };
                    for hook_input in inputs {
                        let Some(path) = hook_input.tool_input.file_path.as_ref() else { continue; };
                        let absolute = PathBuf::from(path);
                        if state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reported.contains(&absolute) { continue; }
                        let input = comment_checker_core::RunCommentCheckerInput { hook_input, binary_path: Some(binary.to_string_lossy().into_owned()), custom_prompt: None };
                        let result = match &check { Some(check) => check(input).await, None => default_run_comment_checker(&input).await }.map_err(|e| ExtensionFailure::new(e.to_string()))?;
                        if let Some(failure) = &result.failure {
                            state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).inert_for_session = true;
                            report_comment_checker_failure(ctx.logger.as_ref(), Some(&ctx.ui), &binary.to_string_lossy(), failure);
                            return Ok(EventResult::None);
                        }
                        let message = normalize_feedback_text(&result.message);
                        if result.has_comments && !message.is_empty() {
                            state.lock().unwrap_or_else(std::sync::PoisonError::into_inner).reported.insert(absolute.clone());
                            content.push(ToolContent::text(format!("{COMMENT_CHECKER_FEEDBACK_HEADER} {}:
{message}", absolute.display())));
                        }
                    }
                    if content.len() == event.content.len() { return Ok(EventResult::None); }
                    Ok(EventResult::ToolResult(ToolResultEventResult {
                        content: Some(content),
                        ..Default::default()
                    }))
                })
            }),
        );
    }
}

fn normalize_path(path: &std::path::Path) -> PathBuf {
    let mut absolute = PathBuf::new();
    for part in path.components() {
        match part {
            std::path::Component::CurDir => {},
            std::path::Component::ParentDir => { absolute.pop(); },
            other => absolute.push(other.as_os_str()),
        }
    }
    absolute
}
