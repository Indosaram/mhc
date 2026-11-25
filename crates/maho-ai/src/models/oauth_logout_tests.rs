use super::*;
use crate::auth::types::{LogoutOptions, OAuthAuth, OAuthRevocationResult};
use crate::providers::chatgpt_subscription::chatgpt_subscription_provider;
use crate::utils::event_stream::create_assistant_message_event_stream;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

const PROVIDER: &str = "chatgpt-subscription";
const DEADLINE: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct Revocation {
    stream_aborted: Option<bool>,
    finish: oneshot::Sender<()>,
}

struct PausedLogout {
    entered: mpsc::UnboundedSender<Revocation>,
    stream_signal: Arc<Mutex<Option<AbortSignal>>>,
}

impl ModelsAuth for PausedLogout {
    fn logout_with_options<'a>(
        &'a self,
        _provider_id: &'a str,
        oauth: Option<Arc<dyn OAuthAuth>>,
        _options: LogoutOptions,
    ) -> BoxFuture<'a, Result<Option<OAuthRevocationResult>, ModelsError>> {
        Box::pin(async move {
            assert!(oauth.is_some_and(|oauth| oauth.has_revoke()));
            let (finish, wait) = oneshot::channel();
            let stream_aborted = self
                .stream_signal
                .lock()
                .expect("signal fixture")
                .as_ref()
                .map(AbortSignal::aborted);
            self.entered
                .send(Revocation {
                    stream_aborted,
                    finish,
                })
                .expect("revocation observer is alive");
            wait.await.expect("release revocation");
            Ok(Some(OAuthRevocationResult::Revoked))
        })
    }

    fn resolve<'a>(
        &'a self,
        _provider: &'a dyn Provider,
        _overrides: &'a AuthResolutionOverrides,
    ) -> BoxFuture<'a, Result<Option<AuthResolution>, ModelsError>> {
        Box::pin(async {
            Ok(Some(AuthResolution {
                ambient: false,
                auth: ProviderAuthResult {
                    api_key: Some("synthetic-session".into()),
                    ..Default::default()
                },
                env: None,
            }))
        })
    }

    fn refresh_credential<'a>(
        &'a self,
        _provider: &'a dyn Provider,
        _stored: Option<&'a Credential>,
        _signal: &'a AbortSignal,
    ) -> BoxFuture<'a, Result<Option<Credential>, ModelsError>> {
        Box::pin(async { Ok(None) })
    }
}

fn models_with_paused_logout(auth: Arc<PausedLogout>) -> Models {
    let models = create_models(Some(CreateModelsOptions {
        auth: Some(auth),
        ..Default::default()
    }));
    models.set_provider(chatgpt_subscription_provider().expect("shipped provider"));
    models
}

#[tokio::test]
async fn keeps_auth_blocked_until_both_overlapping_slot_logouts_finish() {
    tokio::time::timeout(DEADLINE, async {
        let (entered, mut revocations) = mpsc::unbounded_channel();
        let models = models_with_paused_logout(Arc::new(PausedLogout {
            entered,
            stream_signal: Arc::default(),
        }));
        let start_logout = |slot: &str| {
            let models = models.clone();
            let slot_id = Some(slot.to_owned());
            tokio::spawn(async move {
                models
                    .logout_with_options(
                        PROVIDER,
                        Some(LogoutOptions {
                            slot_id,
                            ..Default::default()
                        }),
                    )
                    .await
            })
        };
        let a = start_logout("work");
        let first = revocations.recv().await.expect("first remote revocation");
        let b = start_logout("home");
        let second = revocations.recv().await.expect("second remote revocation");

        first.finish.send(()).expect("release first revocation");
        assert_eq!(
            a.await.expect("first task").expect("logout"),
            Some(OAuthRevocationResult::Revoked)
        );
        let overrides = AuthResolutionOverrides::default();
        assert!(
            models
                .get_auth(PROVIDER, &overrides)
                .await
                .expect("read while signing out")
                .is_none()
        );

        second.finish.send(()).expect("release second revocation");
        assert_eq!(
            b.await.expect("second task").expect("logout"),
            Some(OAuthRevocationResult::Revoked)
        );
        assert!(
            models
                .get_auth(PROVIDER, &overrides)
                .await
                .expect("read after signing out")
                .is_some()
        );
    })
    .await
    .expect("bounded logout test");
}

#[tokio::test]
async fn aborts_an_active_stream_before_starting_remote_revocation() {
    tokio::time::timeout(DEADLINE, async {
        let (entered, mut revocations) = mpsc::unbounded_channel();
        let stream_signal = Arc::new(Mutex::new(None));
        let models = models_with_paused_logout(Arc::new(PausedLogout {
            entered,
            stream_signal: stream_signal.clone(),
        }));
        let model = models
            .get_provider(PROVIDER)
            .expect("provider")
            .get_models()
            .into_iter()
            .next()
            .expect("chat model");
        let (opened, opening) = oneshot::channel();
        let caller = AbortController::new();
        let stream = models.lazy(&model, Some(caller.signal()), |signal| async move {
            *stream_signal.lock().expect("signal fixture") = signal;
            opened.send(()).expect("opening observer alive");
            Ok(create_assistant_message_event_stream())
        });
        opening.await.expect("stream started");

        let (logout, ()) = tokio::join!(models.logout_with_options(PROVIDER, None), async {
            let revocation = revocations.recv().await.expect("remote revocation");
            assert_eq!(revocation.stream_aborted, Some(true));
            assert!(
                !caller.signal().aborted(),
                "logout cancels only the linked request"
            );
            revocation.finish.send(()).expect("release revocation");
        });
        assert_eq!(
            logout.expect("logout"),
            Some(OAuthRevocationResult::Revoked)
        );
        stream.cancel().await.expect("stream cleaned up");
    })
    .await
    .expect("bounded logout test");
}
