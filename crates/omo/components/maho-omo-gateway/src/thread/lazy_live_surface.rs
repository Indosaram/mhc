//! Registration stays synchronous; the surface is constructed on the first operation and rebuilt
//! after a failed one.
//!
//! Upstream counterpart: `thread/lazy-live-surface.ts`.

use std::sync::{Arc, Mutex};

use super::adapter::{GatewayEndpointPort, ReleaseSessionReply, UiAnswer, UiAnswerReply};
use super::live_surface::LiveThreadSurface;

type Loader = Arc<dyn Fn() -> Result<Arc<LiveThreadSurface>, String> + Send + Sync>;

pub struct LazyLiveThreadSurface {
    socket: String,
    load: Loader,
    loaded: Mutex<Option<Arc<LiveThreadSurface>>>,
}

impl LazyLiveThreadSurface {
    pub fn new(socket: impl Into<String>, load: Loader) -> Self {
        Self { socket: socket.into(), load, loaded: Mutex::new(None) }
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
        let socket = self.socket.clone();
        let wake = self.clone_handle();
        let release = self.clone_handle();
        let liveness = self.clone_handle();
        let respond = self.clone_handle();
        let _ = socket;
        GatewayEndpointPort {
            wake: Arc::new(move |endpoint, ids| {
                let handle = wake.clone();
                Box::pin(async move {
                    let surface = match handle.surface() { Ok(surface) => surface, Err(error) => return super::adapter::GatewayWakeReply { admitted: Vec::new(), unroutable: Some(error) } };
                    (surface.gateway().wake)(endpoint, ids).await
                })
            }),
            release_session: Some(Arc::new(move |endpoint, request| {
                let handle = release.clone();
                Box::pin(async move {
                    let Ok(surface) = handle.surface() else {
                        return ReleaseSessionReply::Failure { error: "host_unavailable".to_owned(), error_data: None };
                    };
                    match surface.gateway().release_session {
                        Some(release) => release(endpoint, request).await,
                        None => ReleaseSessionReply::Failure { error: "unsupported:release_session".to_owned(), error_data: None },
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
                        return UiAnswerReply::Refused { error: "host_unavailable".to_owned() };
                    };
                    match surface.gateway().respond_ui {
                        Some(respond) => respond(endpoint, answer).await,
                        None => UiAnswerReply::Refused { error: "unsupported:respondUi".to_owned() },
                    }
                })
            })),
        }
    }

    fn clone_handle(&self) -> Arc<LazyHandle> {
        Arc::new(LazyHandle { socket: self.socket.clone(), load: self.load.clone(), loaded: self.loaded.lock().ok().and_then(|loaded| loaded.clone()) })
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
        assert_eq!(lazy.surface().err().expect("failed"), "host_unavailable:/run/host.sock");
        assert!(lazy.surface().is_err());
        assert_eq!(*attempts.lock().expect("attempts"), 2);
    }
}
