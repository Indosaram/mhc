//! Local SessionControl implementation (`thread/control/local-control.ts`).

use std::sync::Arc;

use serde_json::{Value, json};

use super::super::contracts::results::{ThreadSummary, ThreadWaitOutcome};
use super::super::contracts::state::ThreadModel;
use super::super::endpoint_registry::EndpointKind;
use super::super::reader::ThreadTranscriptEntry;
use super::super::thread_link::thread_link;
use super::super::tools::internals::{routing_id, session_port, summary};
use super::super::tools::ports::{
    BoxFuture, ThreadCloseAnswer, ThreadCloseOptions, ThreadHostSession, ThreadSessionPort,
    ThreadToolSurfaceOptions,
};
use super::super::tools::thread_row::{row_named, thread_row};
use super::super::tools::thread_state::thread_state;
use super::interrupt::{InterruptOutcome, interrupt_over};
use super::port::{
    AvailableModels, ControlError, SESSION_ONLY_THINKING_SCOPE, SessionControl, SessionModelState,
    refuse, session_model_state,
};
use super::wait_settled::wait_for_turn_end;

const THINKING_LEVEL_UNSUPPORTED: &str = "thinking_level_unsupported:";
const ARCHIVE_UNSUPPORTED: &str = "unsupported:set_session_archived";

pub struct LocalControl {
    thread_id: String,
    title: String,
    session: ThreadHostSession,
    port: Arc<dyn ThreadSessionPort>,
    routing: String,
    visible: Arc<dyn Fn() -> Vec<ThreadSummary> + Send + Sync>,
}

pub fn create_local_control(
    options: &ThreadToolSurfaceOptions,
    session: ThreadHostSession,
    thread_id: impl Into<String>,
    title: impl Into<String>,
    visible: Arc<dyn Fn() -> Vec<ThreadSummary> + Send + Sync>,
) -> Arc<dyn SessionControl> {
    let port = session_port(options, &session);
    let routing = routing_id(&session).to_owned();
    Arc::new(LocalControl {
        thread_id: thread_id.into(),
        title: title.into(),
        session,
        port,
        routing,
        visible,
    })
}

impl SessionControl for LocalControl {
    fn thread_id(&self) -> &str {
        &self.thread_id
    }

    fn title(&self) -> &str {
        &self.title
    }

    fn visible(&self) -> BoxFuture<Result<Vec<ThreadSummary>, ControlError>> {
        let v = (self.visible)();
        Box::pin(async move { Ok(v) })
    }

    fn interrupt(
        &self,
        turn_id: Option<&str>,
    ) -> BoxFuture<Result<InterruptOutcome, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        let turn_owned = turn_id.map(str::to_owned);
        Box::pin(async move {
            interrupt_over(
                move |req| port.interrupt(&routing, req, Some(&thread_id)),
                turn_owned.as_deref(),
            )
            .await
            .map_err(ControlError::Transport)
        })
    }

    fn close(
        &self,
        options: ThreadCloseOptions,
    ) -> BoxFuture<Result<ThreadCloseAnswer, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        let is_tui = self.session.endpoint_kind == Some(EndpointKind::Tui);
        Box::pin(async move {
            if is_tui && options.graceful != Some(true) {
                return Err(refuse(
                    "unsupported",
                    format!(
                        "Thread {thread_id} belongs to an interactive terminal, which closes only gracefully."
                    ),
                    "Close it with graceful: true (the terminal exits once its turn ends), or use thread_interrupt (tool.thread_interrupt() in an eval cell) to stop its turn.",
                    Some(json!({ "command": "close_session", "retry_with": { "graceful": true } })),
                )
                .into());
            }
            port.close_session(&routing, Some(&thread_id), options)
                .await
                .map_err(|err| {
                    if err == "unsupported:close_session" {
                        refuse(
                            "unsupported",
                            format!("The host holding thread {thread_id} does not accept close requests."),
                            "Update omo on this device and restart its host.",
                            Some(json!({ "command": "close_session" })),
                        )
                        .into()
                    } else {
                        ControlError::Transport(err)
                    }
                })
        })
    }

    fn set_name(&self, name: &str) -> BoxFuture<Result<(), ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        let name = name.to_owned();
        Box::pin(async move {
            port.set_session_name(&routing, &name, Some(&thread_id))
                .await
                .map_err(ControlError::Transport)
        })
    }

    fn get_available_models(
        &self,
        _fresh: bool,
    ) -> BoxFuture<Result<AvailableModels, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        Box::pin(async move {
            let models = port
                .get_available_models(&routing)
                .await
                .map_err(ControlError::Transport)?;
            Ok(AvailableModels {
                models,
                held: false,
            })
        })
    }

    fn get_model_state(&self) -> BoxFuture<Result<Option<SessionModelState>, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        Box::pin(async move {
            let state = port
                .get_state(&routing, Some(&thread_id))
                .await
                .map_err(ControlError::Transport)?;
            Ok(session_model_state(
                state.model.as_ref(),
                state.thinking_level.as_ref().and_then(Value::as_str),
            ))
        })
    }

    fn set_model(&self, provider: &str, id: &str) -> BoxFuture<Result<ThreadModel, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        let provider = provider.to_owned();
        let id = id.to_owned();
        Box::pin(async move {
            let selected = port
                .set_model(&routing, &provider, &id, Some(&thread_id))
                .await
                .map_err(ControlError::Transport)?;
            Ok(ThreadModel {
                provider: selected.provider,
                id: selected.id,
            })
        })
    }

    fn get_available_thinking_levels(&self) -> BoxFuture<Result<Vec<String>, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        Box::pin(async move {
            port.get_available_thinking_levels(&routing)
                .await
                .map_err(ControlError::Transport)
        })
    }

    fn set_thinking_level(&self, level: &str) -> BoxFuture<Result<(), ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        let level = level.to_owned();
        Box::pin(async move {
            match port
                .set_thinking_level(
                    &routing,
                    &level,
                    Some(SESSION_ONLY_THINKING_SCOPE),
                    Some(&thread_id),
                )
                .await
            {
                Ok(()) => Ok(()),
                Err(err) => {
                    if let Some(rest) = err.strip_prefix(THINKING_LEVEL_UNSUPPORTED) {
                        Err(refuse(
                            "thinking_level_unsupported",
                            rest,
                            "Choose a level from the supported list and retry.",
                            Some(json!({ "level": level })),
                        )
                        .into())
                    } else {
                        Err(ControlError::Transport(err))
                    }
                }
            }
        })
    }

    fn get_messages(&self) -> BoxFuture<Result<Vec<ThreadTranscriptEntry>, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        Box::pin(async move {
            port.get_messages(&routing)
                .await
                .map_err(ControlError::Transport)
        })
    }

    fn wait_settled(&self, timeout_ms: u64) -> BoxFuture<Result<ThreadWaitOutcome, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        let watch_thread_id = thread_id.clone();
        Box::pin(async move {
            wait_for_turn_end(
                move |handlers| port.watch_session(&routing, handlers, Some(&watch_thread_id)),
                |reason| {
                    refuse(
                        "unsupported",
                        format!(
                            "The endpoint holding thread {thread_id} refused to report its status ({reason})."
                        ),
                        "Update omo on this device and restart its host.",
                        Some(json!({ "command": "subscribe_sessions", "reason": reason })),
                    )
                },
                timeout_ms,
            )
            .await
        })
    }

    fn running(&self) -> BoxFuture<Result<bool, ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let archived = self.session.archived;
        let activity = self.session.activity;
        Box::pin(async move {
            if thread_state(archived, true, activity)
                == super::super::contracts::state::ThreadState::Running
            {
                return Ok(true);
            }
            let st = port
                .get_state(&routing, None)
                .await
                .map_err(ControlError::Transport)?;
            Ok(st.is_streaming == Some(true))
        })
    }

    fn set_archived(&self, archived: bool) -> BoxFuture<Result<(), ControlError>> {
        let port = self.port.clone();
        let routing = self.routing.clone();
        let thread_id = self.thread_id.clone();
        Box::pin(async move {
            match port
                .set_session_archived(&routing, archived, Some(&thread_id))
                .await
            {
                Ok(()) => Ok(()),
                Err(err) if err == ARCHIVE_UNSUPPORTED => Err(refuse(
                    "unsupported",
                    format!(
                        "The engine holding thread {thread_id} does not know set_session_archived: it runs a senpi from before archived threads."
                    ),
                    "Update omo on this device and restart the session's host or terminal, then retry.",
                    Some(json!({ "command": "set_session_archived" })),
                )
                .into()),
                Err(err) => Err(ControlError::Transport(err)),
            }
        })
    }

    fn listed(&self, archived: bool) -> BoxFuture<Result<Value, ControlError>> {
        let mut session = self.session.clone();
        session.archived = Some(archived);
        let title = self.title.clone();
        let thread_id = self.thread_id.clone();
        Box::pin(async move {
            let mut sum = row_named(summary(&session, None), Some(&title), None);
            sum.name = title.clone();
            sum.link = thread_link(&thread_id, Some(&title));
            Ok(thread_row(&sum, None))
        })
    }
}
