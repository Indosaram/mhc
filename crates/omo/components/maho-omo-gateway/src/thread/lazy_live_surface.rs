//! Registration stays synchronous; the surface is constructed on the first operation and rebuilt
//! after a failed one.
//!
//! Upstream counterpart: `thread/lazy-live-surface.ts`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use super::adapter::{GatewayEndpointPort, ReleaseSessionReply, UiAnswer, UiAnswerReply};
use super::endpoint_registry::EndpointKind;
use super::live_surface::LiveThreadSurface;
use super::live_surface_paths::resolve_thread_socket;
use super::reader::ThreadTranscriptEntry;
use super::tools::model_match::ModelChoice;
use super::tools::ports::{
    BoxFuture, InterruptRequest, OpenSessionParams, PromptDeliveryOptions, SessionStateSnapshot,
    SessionWatchHandlers, SessionWatchOutcome, ThreadCloseAnswer, ThreadCloseOptions, ThreadHost,
    ThreadHostSession, ThreadHostView, ThreadHostViewRequest, ThreadSessionPort,
};

type Loader = Arc<dyn Fn() -> Result<Arc<LiveThreadSurface>, String> + Send + Sync>;

pub struct LazyLiveThreadSurface {
    socket: String,
    load: Loader,
    loaded: Mutex<Option<Arc<LiveThreadSurface>>>,
}

impl LazyLiveThreadSurface {
    pub fn new(socket: impl Into<String>, load: Loader) -> Self {
        Self {
            socket: socket.into(),
            load,
            loaded: Mutex::new(None),
        }
    }

    pub fn new_for_agent(agent_dir: PathBuf, legacy_socket: Option<String>) -> Self {
        let socket = legacy_socket
            .clone()
            .unwrap_or_else(|| resolve_thread_socket(&agent_dir));
        let sock_copy = socket.clone();
        Self::new(
            socket,
            Arc::new(move || {
                Ok(Arc::new(LiveThreadSurface::new_real(
                    agent_dir.clone(),
                    Some(sock_copy.clone()),
                )))
            }),
        )
    }

    pub fn socket(&self) -> &str {
        &self.socket
    }

    /// Concurrent callers share one attempt; a failed attempt is not cached, so a transient failure
    /// does not last until restart.
    pub fn surface(&self) -> Result<Arc<LiveThreadSurface>, String> {
        if let Ok(loaded) = self.loaded.lock()
            && let Some(surface) = loaded.as_ref()
        {
            return Ok(surface.clone());
        }
        match (*self.load)() {
            Ok(surface) => {
                if let Ok(mut loaded) = self.loaded.lock() {
                    *loaded = Some(surface.clone());
                }
                Ok(surface)
            }
            Err(_error) => Err(format!("host_unavailable:{}", self.socket)),
        }
    }

    /// The gateway port, resolved lazily on every call so a rebuilt surface is picked up.
    pub fn gateway(&self) -> GatewayEndpointPort {
        let wake = self.clone_handle();
        let release = self.clone_handle();
        let liveness = self.clone_handle();
        let respond = self.clone_handle();
        GatewayEndpointPort {
            wake: Arc::new(move |endpoint, ids| {
                let handle = wake.clone();
                Box::pin(async move {
                    let surface = match handle.surface() {
                        Ok(surface) => surface,
                        Err(error) => {
                            return super::adapter::GatewayWakeReply {
                                admitted: Vec::new(),
                                unroutable: Some(error),
                            };
                        }
                    };
                    (surface.gateway().wake)(endpoint, ids).await
                })
            }),
            release_session: Some(Arc::new(move |endpoint, request| {
                let handle = release.clone();
                Box::pin(async move {
                    let Ok(surface) = handle.surface() else {
                        return ReleaseSessionReply::Failure {
                            error: "host_unavailable".to_owned(),
                            error_data: None,
                        };
                    };
                    match surface.gateway().release_session {
                        Some(release) => release(endpoint, request).await,
                        None => ReleaseSessionReply::Failure {
                            error: "unsupported:release_session".to_owned(),
                            error_data: None,
                        },
                    }
                })
            })),
            classify_liveness: Some(Arc::new(move |endpoint| {
                let handle = liveness.clone();
                Box::pin(async move {
                    let Ok(surface) = handle.surface() else {
                        return super::adapter::EndpointLiveness::Dead;
                    };
                    match surface.gateway().classify_liveness {
                        Some(classify) => classify(endpoint).await,
                        None => super::adapter::EndpointLiveness::Dead,
                    }
                })
            })),
            respond_ui: Some(Arc::new(move |endpoint, answer: UiAnswer| {
                let handle = respond.clone();
                Box::pin(async move {
                    let Ok(surface) = handle.surface() else {
                        return UiAnswerReply::Refused {
                            error: "host_unavailable".to_owned(),
                        };
                    };
                    match surface.gateway().respond_ui {
                        Some(respond) => respond(endpoint, answer).await,
                        None => UiAnswerReply::Refused {
                            error: "unsupported:respondUi".to_owned(),
                        },
                    }
                })
            })),
        }
    }

    fn clone_handle(&self) -> Arc<LazyHandle> {
        Arc::new(LazyHandle {
            socket: self.socket.clone(),
            load: self.load.clone(),
            loaded: self.loaded.lock().ok().and_then(|loaded| loaded.clone()),
        })
    }
}

impl ThreadSessionPort for LazyLiveThreadSurface {
    fn get_messages(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<ThreadTranscriptEntry>, String>> {
        let sid = session_id.to_owned();
        let surf = self.surface();
        Box::pin(async move { surf?.get_messages(&sid).await })
    }
    fn get_state(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionStateSnapshot, String>> {
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move { surf?.get_state(&sid, did.as_deref()).await })
    }
    fn prompt(
        &self,
        session_id: &str,
        message: &str,
        options: Option<PromptDeliveryOptions>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Option<String>, String>> {
        let sid = session_id.to_owned();
        let msg = message.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move { surf?.prompt(&sid, &msg, options, did.as_deref()).await })
    }
    fn interrupt(
        &self,
        session_id: &str,
        request: InterruptRequest,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<Value, String>> {
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move { surf?.interrupt(&sid, request, did.as_deref()).await })
    }
    fn set_session_name(
        &self,
        session_id: &str,
        name: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        let sid = session_id.to_owned();
        let name = name.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move { surf?.set_session_name(&sid, &name, did.as_deref()).await })
    }
    fn set_session_archived(
        &self,
        session_id: &str,
        archived: bool,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move {
            surf?
                .set_session_archived(&sid, archived, did.as_deref())
                .await
        })
    }
    fn set_model(
        &self,
        session_id: &str,
        provider: &str,
        model_id: &str,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<ModelChoice, String>> {
        let sid = session_id.to_owned();
        let p = provider.to_owned();
        let m = model_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move { surf?.set_model(&sid, &p, &m, did.as_deref()).await })
    }
    fn get_available_models(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<ModelChoice>, String>> {
        let sid = session_id.to_owned();
        let surf = self.surface();
        Box::pin(async move { surf?.get_available_models(&sid).await })
    }
    fn set_thinking_level(
        &self,
        session_id: &str,
        level: &str,
        scope: Option<&str>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<(), String>> {
        let sid = session_id.to_owned();
        let lvl = level.to_owned();
        let sc = scope.map(str::to_owned);
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move {
            surf?
                .set_thinking_level(&sid, &lvl, sc.as_deref(), did.as_deref())
                .await
        })
    }
    fn get_available_thinking_levels(
        &self,
        session_id: &str,
    ) -> BoxFuture<Result<Vec<String>, String>> {
        let sid = session_id.to_owned();
        let surf = self.surface();
        Box::pin(async move { surf?.get_available_thinking_levels(&sid).await })
    }
    fn close_session(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
        options: ThreadCloseOptions,
    ) -> BoxFuture<Result<ThreadCloseAnswer, String>> {
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move { surf?.close_session(&sid, did.as_deref(), options).await })
    }
    fn watch_session(
        &self,
        session_id: &str,
        handlers: Arc<dyn SessionWatchHandlers>,
        durable_session_id: Option<&str>,
    ) -> BoxFuture<Result<SessionWatchOutcome, String>> {
        let sid = session_id.to_owned();
        let did = durable_session_id.map(str::to_owned);
        let surf = self.surface();
        Box::pin(async move { surf?.watch_session(&sid, handlers, did.as_deref()).await })
    }
}

impl ThreadHost for LazyLiveThreadSurface {
    fn socket(&self) -> &str {
        &self.socket
    }
    fn list_sessions(&self) -> BoxFuture<Result<Vec<ThreadHostSession>, String>> {
        let surf = self.surface();
        Box::pin(async move { surf?.list_sessions().await })
    }
    fn open_session(
        &self,
        params: OpenSessionParams,
    ) -> BoxFuture<Result<ThreadHostSession, String>> {
        let surf = self.surface();
        Box::pin(async move { ThreadHost::open_session(&*surf?, params).await })
    }
    fn list_view(
        &self,
        request: ThreadHostViewRequest,
    ) -> BoxFuture<Result<Option<ThreadHostView>, String>> {
        let surf = self.surface();
        Box::pin(async move { ThreadHost::list_view(&*surf?, request).await })
    }
    fn list_target(
        &self,
        durable_id: &str,
        endpoint_socket: &str,
        endpoint_kind: EndpointKind,
    ) -> BoxFuture<Result<Option<ThreadHostView>, String>> {
        let did = durable_id.to_owned();
        let sock = endpoint_socket.to_owned();
        let surf = self.surface();
        Box::pin(async move { ThreadHost::list_target(&*surf?, &did, &sock, endpoint_kind).await })
    }
    fn endpoint(&self, socket: &str) -> Option<Arc<dyn ThreadSessionPort>> {
        let surf = self.surface().ok()?;
        surf.endpoint(socket)
    }
    fn gateway(&self) -> Option<GatewayEndpointPort> {
        Some(LazyLiveThreadSurface::gateway(self))
    }
}

struct LazyHandle {
    socket: String,
    load: Loader,
    loaded: Option<Arc<LiveThreadSurface>>,
}

impl LazyHandle {
    fn surface(&self) -> Result<Arc<LiveThreadSurface>, String> {
        if let Some(surface) = &self.loaded {
            return Ok(surface.clone());
        }
        (*self.load)().map_err(|_| format!("host_unavailable:{}", self.socket))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_load_is_not_cached_and_reads_as_host_unavailable() {
        let attempts = Arc::new(Mutex::new(0));
        let lazy = LazyLiveThreadSurface::new(
            "/run/host.sock",
            Arc::new({
                let attempts = attempts.clone();
                move || {
                    *attempts.lock().expect("attempts") += 1;
                    Err("import failed".to_owned())
                }
            }),
        );
        assert_eq!(
            lazy.surface().err().expect("failed"),
            "host_unavailable:/run/host.sock"
        );
        assert!(lazy.surface().is_err());
        assert_eq!(*attempts.lock().expect("attempts"), 2);
    }
}
