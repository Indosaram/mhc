//! Port of senpi packages/coding-agent/src/core/session-control-actions.ts.
//!
//! `pi.session` as one session binds it: admission, the ledger and the header write come from the
//! session itself (`base`), and the control endpoint from whichever host the running mode installed.
//! Only the interactive TUI installs one; every other mode answers `unsupported` and registers nothing.
//! The endpoints are owned by one extension generation, which a reload retires (senpi#3246).

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use maho_ext_api::AbortSignal;
use serde_json::Value;

use crate::extensions::session_control_types::{
    ControlEndpointUnsupported, DisposeControlEndpoint, RegisterControlEndpointOptions,
    SessionControlActions, SessionControlRegistration,
};

/// The mode's endpoint host (senpi `ControlEndpointHost`).
pub trait ControlEndpointHost: Send + Sync {
    fn register(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> BoxFuture<'_, SessionControlRegistration>;
}

pub type ControlEndpointHostProvider =
    Arc<dyn Fn() -> Option<Arc<dyn ControlEndpointHost>> + Send + Sync>;

pub struct SessionControlActionDeps {
    /// Admission, ledger, header write and relocation of the bound session.
    pub base: Arc<dyn SessionControlActions>,
    pub host: ControlEndpointHostProvider,
}

/// One extension generation's `pi.session`, which owns the control endpoints that generation registered.
pub struct GenerationSessionControlActions {
    deps: SessionControlActionDeps,
    owned: Arc<Mutex<Vec<(u64, DisposeControlEndpoint)>>>,
    next_id: AtomicU64,
    retired: Arc<AtomicBool>,
}

pub fn create_session_control_actions(
    deps: SessionControlActionDeps,
) -> GenerationSessionControlActions {
    GenerationSessionControlActions {
        deps,
        owned: Arc::new(Mutex::new(Vec::new())),
        next_id: AtomicU64::new(0),
        retired: Arc::new(AtomicBool::new(false)),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl GenerationSessionControlActions {
    /// Disposes every endpoint this generation registered. A reload calls it as it retires the
    /// generation: the endpoint's `drain` is that generation's closure, so it must not outlive it
    /// (senpi#3246). Idempotent.
    pub async fn retire(&self) -> Result<(), String> {
        // `retired` is set under the `owned` lock, the same guard register() checks it under, so a
        // registration either lands in the list taken here or sees `retired` and tears itself down.
        let registrations = {
            let mut owned = lock(&self.owned);
            self.retired.store(true, Ordering::SeqCst);
            std::mem::take(&mut *owned)
        };
        let results =
            futures::future::join_all(registrations.into_iter().map(|(_, dispose)| dispose()))
                .await;
        results.into_iter().collect()
    }

    async fn register(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> SessionControlRegistration {
        let Some(host) = (self.deps.host)() else {
            return SessionControlRegistration::Unsupported {
                reason: ControlEndpointUnsupported::UnsupportedMode,
            };
        };
        // The endpoint may run a pass before this registration returns (its first `inbox` wake): a retired
        // generation's `drain` is never called, whatever the timing.
        let retired = Arc::clone(&self.retired);
        let drain = Arc::clone(&options.drain);
        let registration = host
            .register(RegisterControlEndpointOptions {
                drain: Arc::new(
                    move |event: Value| -> BoxFuture<'static, Result<Option<Value>, String>> {
                        if retired.load(Ordering::SeqCst) {
                            Box::pin(async { Ok(None) })
                        } else {
                            drain(event)
                        }
                    },
                ),
                ..options
            })
            .await;
        let SessionControlRegistration::Registered { socket, dispose } = registration else {
            return registration;
        };
        // The retired check and the push happen under one guard (no await while it is held): retire()
        // sets `retired` under the same lock, so it either takes this entry or this sees `retired`.
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let stale = {
            let mut owned = lock(&self.owned);
            let stale = self.retired.load(Ordering::SeqCst);
            if !stale {
                owned.push((id, Arc::clone(&dispose)));
            }
            stale
        };
        // Registered while this generation was being retired: it is released at once (outside the
        // lock), never left serving.
        if stale {
            let teardown = match dispose().await {
                Ok(()) => String::new(),
                Err(error) => format!("; its teardown failed: {error}"),
            };
            return SessionControlRegistration::Failed {
                reason: format!("stale extension generation after reload{teardown}"),
            };
        }
        let owned = Arc::clone(&self.owned);
        SessionControlRegistration::Registered {
            socket,
            dispose: Arc::new(move || -> BoxFuture<'static, Result<(), String>> {
                lock(&owned).retain(|(owned_id, _)| *owned_id != id);
                dispose()
            }),
        }
    }
}

impl SessionControlActions for GenerationSessionControlActions {
    fn register_control_endpoint(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> BoxFuture<'_, SessionControlRegistration> {
        Box::pin(self.register(options))
    }
    fn admission_gate(&self) -> Value {
        self.deps.base.admission_gate()
    }
    fn admit_external_message(&self, input: Value) -> Result<Value, String> {
        self.deps.base.admit_external_message(input)
    }
    fn list_admitted_deliveries(&self) -> Value {
        self.deps.base.list_admitted_deliveries()
    }
    fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>> {
        self.deps.base.persist_header_now()
    }
    fn relocation_offered(&self) -> bool {
        self.deps.base.relocation_offered()
    }
    fn relocation_targets(
        &self,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        self.deps.base.relocation_targets(signal)
    }
    fn request_relocation(
        &self,
        request: Value,
        signal: Option<AbortSignal>,
    ) -> BoxFuture<'_, Result<Value, String>> {
        self.deps.base.request_relocation(request, signal)
    }
}

const NOT_BOUND: &str =
    "Extension runtime not initialized. pi.session cannot be used during extension loading.";

/// The actions `pi.session` exposes before the extension runtime is bound: every call fails.
pub struct UnboundSessionControlActions;

impl SessionControlActions for UnboundSessionControlActions {
    fn register_control_endpoint(
        &self,
        _options: RegisterControlEndpointOptions,
    ) -> BoxFuture<'_, SessionControlRegistration> {
        Box::pin(async {
            SessionControlRegistration::Failed {
                reason: "Extension runtime not initialized".to_owned(),
            }
        })
    }

    fn admission_gate(&self) -> Value {
        Value::Null
    }

    fn admit_external_message(&self, _input: Value) -> Result<Value, String> {
        Err(NOT_BOUND.to_owned())
    }

    fn list_admitted_deliveries(&self) -> Value {
        Value::Null
    }

    fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>> {
        Box::pin(async { Err("Extension runtime not initialized".to_owned()) })
    }
}

pub fn unbound_session_control_actions() -> UnboundSessionControlActions {
    UnboundSessionControlActions
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    struct Base;
    impl SessionControlActions for Base {
        fn admission_gate(&self) -> Value {
            json!({"can_admit":true})
        }
        fn admit_external_message(&self, _input: Value) -> Result<Value, String> {
            Ok(json!({}))
        }
        fn list_admitted_deliveries(&self) -> Value {
            json!({"pending":[],"emitted":[]})
        }
        fn persist_header_now(&self) -> BoxFuture<'_, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[derive(Default)]
    struct Host {
        disposed: Arc<AtomicUsize>,
        drains: Mutex<Vec<crate::extensions::session_control_types::SessionControlDrain>>,
        dispose_error: Option<String>,
        /// Retires this generation while the host is registering (the late-registration race).
        retire_during: Mutex<Option<Arc<GenerationSessionControlActions>>>,
    }
    impl ControlEndpointHost for Host {
        fn register(
            &self,
            options: RegisterControlEndpointOptions,
        ) -> BoxFuture<'_, SessionControlRegistration> {
            Box::pin(async move {
                lock(&self.drains).push(Arc::clone(&options.drain));
                let generation = lock(&self.retire_during).take();
                if let Some(generation) = generation {
                    generation.retire().await.expect("retire");
                }
                let disposed = Arc::clone(&self.disposed);
                let error = self.dispose_error.clone();
                SessionControlRegistration::Registered {
                    socket: "/tmp/control.sock".into(),
                    dispose: Arc::new(move || -> BoxFuture<'static, Result<(), String>> {
                        disposed.fetch_add(1, Ordering::SeqCst);
                        let error = error.clone();
                        Box::pin(async move { error.map_or(Ok(()), Err) })
                    }),
                }
            })
        }
    }

    fn options(drained: Arc<AtomicUsize>) -> RegisterControlEndpointOptions {
        RegisterControlEndpointOptions {
            inbox_dir: "/tmp/inbox".into(),
            drain: Arc::new(
                move |_event: Value| -> BoxFuture<'static, Result<Option<Value>, String>> {
                    drained.fetch_add(1, Ordering::SeqCst);
                    Box::pin(async { Ok(None) })
                },
            ),
            is_session_referenced: None,
        }
    }

    fn generation(host: Option<Arc<Host>>) -> Arc<GenerationSessionControlActions> {
        Arc::new(create_session_control_actions(SessionControlActionDeps {
            base: Arc::new(Base),
            host: Arc::new(move || host.clone().map(|h| h as Arc<dyn ControlEndpointHost>)),
        }))
    }

    #[tokio::test]
    async fn without_a_host_registration_is_unsupported_mode_on_every_platform() {
        let actions = generation(None);
        let result = actions
            .register_control_endpoint(options(Arc::new(AtomicUsize::new(0))))
            .await;
        assert_eq!(
            result.to_json(),
            json!({"status":"unsupported","reason":"unsupported_mode"})
        );
    }

    #[tokio::test]
    async fn retire_disposes_every_endpoint_and_a_retired_drain_is_never_called() {
        let host = Arc::new(Host::default());
        let actions = generation(Some(Arc::clone(&host)));
        let drained = Arc::new(AtomicUsize::new(0));
        let first = actions
            .register_control_endpoint(options(Arc::clone(&drained)))
            .await;
        assert!(matches!(
            first,
            SessionControlRegistration::Registered { .. }
        ));
        let drain = Arc::clone(&lock(&host.drains)[0]);
        drain(json!({"type":"session_control_wake"}))
            .await
            .expect("drain");
        assert_eq!(drained.load(Ordering::SeqCst), 1);
        actions.retire().await.expect("retire");
        assert_eq!(host.disposed.load(Ordering::SeqCst), 1);
        drain(json!({"type":"session_control_wake"}))
            .await
            .expect("drain");
        assert_eq!(
            drained.load(Ordering::SeqCst),
            1,
            "a retired generation's drain never runs"
        );
        actions.retire().await.expect("idempotent");
        assert_eq!(host.disposed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_explicitly_disposed_endpoint_is_not_disposed_again_on_retire() {
        let host = Arc::new(Host::default());
        let actions = generation(Some(Arc::clone(&host)));
        let SessionControlRegistration::Registered { dispose, .. } = actions
            .register_control_endpoint(options(Arc::new(AtomicUsize::new(0))))
            .await
        else {
            panic!("registered");
        };
        dispose().await.expect("dispose");
        actions.retire().await.expect("retire");
        assert_eq!(host.disposed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_registration_that_completes_after_retire_is_disposed_and_fails() {
        let host = Arc::new(Host {
            dispose_error: Some("socket busy".into()),
            ..Host::default()
        });
        let actions = generation(Some(Arc::clone(&host)));
        *lock(&host.retire_during) = Some(Arc::clone(&actions));
        let result = actions
            .register_control_endpoint(options(Arc::new(AtomicUsize::new(0))))
            .await;
        assert_eq!(
            result.to_json(),
            json!({"status":"failed","reason":"stale extension generation after reload; its teardown failed: socket busy"})
        );
        assert_eq!(host.disposed.load(Ordering::SeqCst), 1);
    }

    /// Host whose registration parks until released, so the test fixes when retire() runs.
    struct GatedHost {
        disposed: Arc<AtomicUsize>,
        entered: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
        release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
    }
    impl ControlEndpointHost for GatedHost {
        fn register(
            &self,
            _options: RegisterControlEndpointOptions,
        ) -> BoxFuture<'_, SessionControlRegistration> {
            let entered = lock(&self.entered).take().expect("entered");
            let release = lock(&self.release).take().expect("release");
            let disposed = Arc::clone(&self.disposed);
            Box::pin(async move {
                entered.send(()).expect("test waits for entry");
                release.await.expect("test releases the host");
                SessionControlRegistration::Registered {
                    socket: "/tmp/control.sock".into(),
                    dispose: Arc::new(move || -> BoxFuture<'static, Result<(), String>> {
                        disposed.fetch_add(1, Ordering::SeqCst);
                        Box::pin(async { Ok(()) })
                    }),
                }
            })
        }
    }

    #[tokio::test]
    async fn an_endpoint_cannot_survive_a_retire_that_lands_mid_registration() {
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let host = Arc::new(GatedHost {
            disposed: Arc::new(AtomicUsize::new(0)),
            entered: Mutex::new(Some(entered_tx)),
            release: Mutex::new(Some(release_rx)),
        });
        let provided = Arc::clone(&host);
        let actions = Arc::new(create_session_control_actions(SessionControlActionDeps {
            base: Arc::new(Base),
            host: Arc::new(move || Some(Arc::clone(&provided) as Arc<dyn ControlEndpointHost>)),
        }));
        // Forced order: the host has accepted the registration, retire() runs to completion, and only
        // then does the host hand back the endpoint for register() to record.
        let retirer = async {
            entered_rx.await.expect("host entered");
            actions.retire().await.expect("retire");
            release_tx.send(()).expect("register is parked");
        };
        let (result, ()) = futures::join!(
            actions.register_control_endpoint(options(Arc::new(AtomicUsize::new(0)))),
            retirer
        );
        assert_eq!(
            result.to_json(),
            json!({"status":"failed","reason":"stale extension generation after reload"})
        );
        assert_eq!(host.disposed.load(Ordering::SeqCst), 1, "torn down once");
        assert!(lock(&actions.owned).is_empty(), "nothing left registered");
        actions.retire().await.expect("idempotent");
        assert_eq!(host.disposed.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn unbound_actions_refuse_every_call() {
        let actions = unbound_session_control_actions();
        assert_eq!(
            actions.admit_external_message(Value::Null),
            Err(NOT_BOUND.to_owned())
        );
        assert!(actions.persist_header_now().await.is_err());
        assert!(!actions.relocation_offered());
        let result = actions
            .register_control_endpoint(options(Arc::new(AtomicUsize::new(0))))
            .await;
        assert_eq!(
            result.to_json(),
            json!({"status":"failed","reason":"Extension runtime not initialized"})
        );
    }
}
